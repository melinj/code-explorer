//! Repository-graph operations for the Graph view: bookmarks, landing a stack on a trunk bookmark
//! and the common history edits (`jj rebase`, `describe`, `abandon`, `new --no-edit`, `commit`,
//! `squash`). Each one is a
//! [`MutationPlan::Graph`], so it runs like an edit: one jj operation through
//! [`JjWriter::mutate`] (affected workspaces snapshotted first, their new `@` checked out after),
//! an exact [`JjWriter::preview`], and `jj op revert` to take it back.
//!
//! None of them moves the serving workspace's `@` to another commit on purpose (`jj new` here is
//! `--no-edit`): the files `cv serve` reads stay where the human left them. `jj commit` is the one
//! that starts a new `@`, on top of the old one with the same files, so nothing on disk changes.

use std::collections::{BTreeMap, BTreeSet};

use jj_lib::backend::CommitId as JjCommitId;
use jj_lib::commit::Commit;
use jj_lib::object_id::ObjectId;
use jj_lib::op_store::RefTarget;
use jj_lib::ref_name::{RefNameBuf, WorkspaceNameBuf};
use jj_lib::repo::{ReadonlyRepo, Repo};
use jj_lib::rewrite::{
    CommitWithSelection, MoveCommitsLocation, MoveCommitsTarget, RebaseOptions, merge_commit_trees,
    move_commits, rebase_commit, squash_commits,
};
use jj_lib::transaction::Transaction;

use super::{JjWriter, MutationPlan, block, parse_commit_id};
use crate::{Error, Result, err};

/// How [`GraphOp::Land`] brings a stack onto the trunk bookmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandStrategy {
    /// Move the trunk bookmark forward to the head; refused unless the trunk is an ancestor.
    FastForward,
    /// `jj rebase -b <head> -d <trunk>`, then move the trunk bookmark to the rebased head.
    Rebase,
    /// `jj new --no-edit <trunk> <head>` (a merge commit), then move the trunk bookmark to it.
    Merge,
}

/// Which commits [`GraphOp::Rebase`] moves (`jj rebase -r | -s | -b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseMode {
    /// Only the commit; its descendants move onto its parents.
    Revision,
    /// The commit and its descendants.
    Source,
    /// The whole branch relative to the destination: `roots(dest..source)::`.
    Branch,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GraphOp {
    /// `jj bookmark set <name> -r <to>`: creates the bookmark or moves it. A move to a commit that
    /// is not a descendant of where it points needs `allow_backwards` (as in jj).
    SetBookmark {
        name: String,
        to: String,
        allow_backwards: bool,
    },
    /// `jj bookmark delete <name>` (a tracked remote bookmark is deleted on the next push).
    DeleteBookmark { name: String },
    /// Land the stack ending at `head` on the local bookmark `trunk`. `carry` is an empty working
    /// copy commit on top of `head` (the workspace's `@`) that moves onto the new trunk with a
    /// merge. `delete_bookmark` is deleted afterwards (the landed feature bookmark).
    Land {
        head: String,
        trunk: String,
        strategy: LandStrategy,
        message: Option<String>,
        carry: Option<String>,
        delete_bookmark: Option<String>,
    },
    /// `jj rebase -r|-s|-b <source> -d <dest>…`.
    Rebase {
        source: String,
        mode: RebaseMode,
        dest: Vec<String>,
    },
    /// `jj describe -r <commit> -m <message>`.
    Describe { commit: String, message: String },
    /// `jj abandon <commits>…`: descendants move onto the parents; a workspace whose `@` is
    /// abandoned gets a new empty `@` there.
    Abandon { commits: Vec<String> },
    /// `jj new --no-edit <parents>… [-m <message>]`: a new change, nobody's `@`.
    New {
        parents: Vec<String>,
        message: Option<String>,
    },
    /// `jj commit -m <message>` in `workspace`: its `@` gets the message and a new empty `@` starts
    /// on top (every workspace on that `@` moves, as in jj). `bookmark`, if given, is created or
    /// moved forward to the committed change.
    Commit {
        workspace: String,
        message: String,
        bookmark: Option<String>,
    },
    /// `jj squash --from <source> --into <into> -m <message>`: the source's changes move into
    /// `into` (default: its only parent) and the emptied source is abandoned (a working copy gets a
    /// new empty `@` there). Without `message` the two descriptions are combined as jj does.
    Squash {
        source: String,
        into: Option<String>,
        message: Option<String>,
    },
}

impl GraphOp {
    /// The same operation with every commit id passed through `f`.
    pub(super) fn map_commits(&self, f: &dyn Fn(&str) -> String) -> GraphOp {
        let all = |v: &[String]| v.iter().map(|c| f(c)).collect::<Vec<_>>();
        match self.clone() {
            GraphOp::SetBookmark {
                name,
                to,
                allow_backwards,
            } => GraphOp::SetBookmark {
                name,
                to: f(&to),
                allow_backwards,
            },
            GraphOp::Land {
                head,
                trunk,
                strategy,
                message,
                carry,
                delete_bookmark,
            } => GraphOp::Land {
                head: f(&head),
                trunk,
                strategy,
                message,
                carry: carry.map(|c| f(&c)),
                delete_bookmark,
            },
            GraphOp::Rebase { source, mode, dest } => GraphOp::Rebase {
                source: f(&source),
                mode,
                dest: all(&dest),
            },
            GraphOp::Describe { commit, message } => GraphOp::Describe {
                commit: f(&commit),
                message,
            },
            GraphOp::Abandon { commits } => GraphOp::Abandon {
                commits: all(&commits),
            },
            GraphOp::New { parents, message } => GraphOp::New {
                parents: all(&parents),
                message,
            },
            GraphOp::Squash {
                source,
                into,
                message,
            } => GraphOp::Squash {
                source: f(&source),
                into: into.map(|c| f(&c)),
                message,
            },
            op @ (GraphOp::DeleteBookmark { .. } | GraphOp::Commit { .. }) => op,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            GraphOp::SetBookmark { .. } => "bookmark set",
            GraphOp::DeleteBookmark { .. } => "bookmark delete",
            GraphOp::Land { .. } => "land",
            GraphOp::Rebase { .. } => "rebase",
            GraphOp::Describe { .. } => "describe",
            GraphOp::Abandon { .. } => "abandon",
            GraphOp::New { .. } => "new",
            GraphOp::Commit { .. } => "commit",
            GraphOp::Squash { .. } => "squash",
        }
    }
}

/// A local bookmark and the remote bookmarks of the same name (`@git` left out).
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkAt {
    pub name: String,
    /// Commit ids; more than one when the bookmark is conflicted, none when it is deleted locally
    /// but still tracked on a remote.
    pub targets: Vec<String>,
    pub remotes: Vec<RemoteBookmarkAt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBookmarkAt {
    pub remote: String,
    pub targets: Vec<String>,
    pub tracked: bool,
}

/// Bookmark names jj's revset language can spell without quotes, and nothing that reads as an
/// option, a remote (`name@remote`) or a pattern (`glob:`).
pub(super) fn bookmark_name_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('/')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+'))
}

fn ref_name(name: &str) -> RefNameBuf {
    RefNameBuf::from(name)
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

/// The default description of a landing merge.
fn merge_message(trunk: &str, head: &Commit) -> String {
    let title = head.description().lines().next().unwrap_or("").trim();
    if title.is_empty() {
        format!(
            "Merge {} into {trunk}\n",
            short(&head.change_id().reverse_hex())
        )
    } else {
        format!("Merge \"{title}\" into {trunk}\n")
    }
}

/// The description of a squash's result without an explicit message, as `jj squash` leaves it
/// when its editor changes nothing: one side's when the other is empty, else destination, a blank
/// line, source.
pub fn squash_message(dest: &str, source: &str) -> String {
    match (dest.trim().is_empty(), source.trim().is_empty()) {
        (_, true) => dest.to_string(),
        (true, false) => source.to_string(),
        (false, false) => format!("{}\n\n{}", dest.trim_end(), with_newline(source.trim_end())),
    }
}

fn with_newline(m: &str) -> String {
    if m.ends_with('\n') {
        m.to_string()
    } else {
        format!("{m}\n")
    }
}

impl JjWriter {
    /// Every local bookmark (and the remote bookmarks of the same names) at head, sorted by name.
    pub fn bookmarks(&self) -> Result<Vec<BookmarkAt>> {
        let repo = self.head()?;
        let view = repo.view();
        let mut out: BTreeMap<String, BookmarkAt> = BTreeMap::new();
        let ids = |t: &RefTarget| t.added_ids().map(|id| id.hex()).collect::<Vec<_>>();
        for (name, target) in view.local_bookmarks() {
            out.insert(
                name.as_str().to_string(),
                BookmarkAt {
                    name: name.as_str().to_string(),
                    targets: ids(target),
                    remotes: vec![],
                },
            );
        }
        for (sym, remote) in view.all_remote_bookmarks() {
            if sym.remote.as_str() == jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO.as_str() {
                continue;
            }
            let b = out
                .entry(sym.name.as_str().to_string())
                .or_insert_with(|| BookmarkAt {
                    name: sym.name.as_str().to_string(),
                    targets: vec![],
                    remotes: vec![],
                });
            b.remotes.push(RemoteBookmarkAt {
                remote: sym.remote.as_str().to_string(),
                targets: ids(&remote.target),
                tracked: remote.is_tracked(),
            });
        }
        Ok(out.into_values().collect())
    }

    /// Whether the repo is colocated with git (bookmark changes show up as git branches).
    pub fn colocated(&self) -> bool {
        self.root.join(".git").exists()
            && self
                .head()
                .is_ok_and(|r| jj_lib::git::get_git_backend(r.store()).is_ok())
    }

    /// What landing a workspace whose `@` is `wc` means: an empty, undescribed `@` with one parent
    /// is a scratch commit, so its parent is the head that lands and `@` is carried along
    /// (`(head, Some(wc))`); otherwise `@` itself lands (`(wc, None)`).
    pub fn stack_head(&self, wc: &str) -> Result<(String, Option<String>)> {
        let repo = self.head()?;
        let c = repo
            .store()
            .get_commit(&parse_commit_id(wc)?)
            .map_err(err)?;
        let empty = block(c.is_empty(repo.as_ref())).map_err(err)?;
        if empty && c.description().trim().is_empty() && c.parent_ids().len() == 1 {
            Ok((c.parent_ids()[0].hex(), Some(c.id().hex())))
        } else {
            Ok((c.id().hex(), None))
        }
    }

    /// The single commit a local bookmark points to.
    fn bookmark_target(&self, repo: &dyn Repo, name: &str) -> Result<JjCommitId> {
        let target = repo.view().get_local_bookmark(&ref_name(name));
        if target.is_absent() {
            return Err(Error(format!("no bookmark named {name}")));
        }
        target
            .as_normal()
            .cloned()
            .ok_or_else(|| Error(format!("bookmark {name} is conflicted; set it first")))
    }

    fn is_ancestor(&self, repo: &dyn Repo, ancestor: &JjCommitId, of: &JjCommitId) -> Result<bool> {
        Ok(!self
            .eval_in(repo, &format!("{} & ::{}", ancestor.hex(), of.hex()))?
            .is_empty())
    }

    /// `hex`, or the visible commit of its change when `hex` was rewritten (by the snapshot of a
    /// workspace before a mutation: a plan naming `@` then means the `@` with the files on disk).
    pub(super) fn follow_rewrite(&self, repo: &ReadonlyRepo, hex: &str) -> String {
        let Ok(id) = parse_commit_id(hex) else {
            return hex.to_string();
        };
        let Ok(c) = repo.store().get_commit(&id) else {
            return hex.to_string();
        };
        match block(repo.resolve_change_id(c.change_id())) {
            Ok(Some(t)) => {
                let visible: Vec<_> = t.visible_with_offsets().map(|(_, id)| id).collect();
                match visible.as_slice() {
                    [one] if !visible.contains(&&id) => one.hex(),
                    _ => hex.to_string(),
                }
            }
            _ => hex.to_string(),
        }
    }

    /// A squash's source, destination and resulting description.
    pub(super) fn squash_parts(
        &self,
        repo: &dyn Repo,
        source: &str,
        into: Option<&str>,
        message: Option<&str>,
    ) -> Result<(Commit, Commit, String)> {
        let get = |hex: &str| {
            repo.store()
                .get_commit(&parse_commit_id(hex)?)
                .map_err(|e| Error(format!("unknown commit {hex}: {e}")))
        };
        let src = get(source)?;
        let dest = match into {
            Some(d) => get(d)?,
            None => match src.parent_ids() {
                [p] => repo.store().get_commit(p).map_err(err)?,
                _ => {
                    return Err(Error(format!(
                        "{} has several parents: pick the change to squash it into",
                        short(&src.change_id().reverse_hex())
                    )));
                }
            },
        };
        let m = match message.filter(|m| !m.trim().is_empty()) {
            Some(m) => with_newline(m),
            None => squash_message(dest.description(), src.description()),
        };
        Ok((src, dest, m))
    }

    /// Refuses when any commit of `revset` is immutable, naming the first.
    fn must_be_mutable(&self, repo: &dyn Repo, revset: &str, what: &str) -> Result<()> {
        if let Some(id) = self
            .eval_in(repo, &format!("({revset}) & immutable()"))?
            .first()
        {
            let c = repo.store().get_commit(id).map_err(err)?;
            return Err(Error(format!(
                "{what} would rewrite {}, which is immutable",
                short(&c.change_id().reverse_hex())
            )));
        }
        Ok(())
    }

    /// The checks of [`Self::validate`] for graph operations: user errors, never retried
    /// through the CLI.
    pub(super) fn validate_graph(&self, repo: &ReadonlyRepo, op: &GraphOp) -> Result<()> {
        let commit = |hex: &str| {
            repo.store()
                .get_commit(&parse_commit_id(hex)?)
                .map_err(|e| Error(format!("unknown commit {hex}: {e}")))
        };
        let name_ok = |n: &str| {
            if bookmark_name_ok(n) {
                Ok(())
            } else {
                Err(Error(format!("{n:?} is not a bookmark name cv writes")))
            }
        };
        match op {
            GraphOp::SetBookmark {
                name,
                to,
                allow_backwards,
            } => {
                name_ok(name)?;
                let to = commit(to)?;
                if *to.id() == *repo.store().root_commit_id() {
                    return Err(Error("a bookmark cannot point at the root commit".into()));
                }
                let current = repo.view().get_local_bookmark(&ref_name(name));
                if let Some(old) = current.as_normal()
                    && !allow_backwards
                    && !self.is_ancestor(repo, old, to.id())?
                {
                    return Err(Error(format!(
                        "moving {name} from {} to {} goes backwards or sideways; allow it to move it anyway",
                        short(&old.hex()),
                        short(&to.id().hex())
                    )));
                }
            }
            GraphOp::DeleteBookmark { name } => {
                name_ok(name)?;
                if repo.view().get_local_bookmark(&ref_name(name)).is_absent() {
                    return Err(Error(format!("no bookmark named {name}")));
                }
            }
            GraphOp::Land {
                head,
                trunk,
                strategy,
                carry,
                delete_bookmark,
                ..
            } => {
                name_ok(trunk)?;
                let head = commit(head)?;
                let t = self.bookmark_target(repo, trunk)?;
                if self.is_ancestor(repo, head.id(), &t)? {
                    return Err(Error(format!(
                        "{} is already in {trunk}",
                        short(&head.change_id().reverse_hex())
                    )));
                }
                let ff = self.is_ancestor(repo, &t, head.id())?;
                match strategy {
                    LandStrategy::FastForward if !ff => {
                        return Err(Error(format!(
                            "{trunk} is not an ancestor of {}: it cannot fast-forward; rebase or merge instead",
                            short(&head.change_id().reverse_hex())
                        )));
                    }
                    LandStrategy::Rebase if !ff => self.must_be_mutable(
                        repo,
                        &format!("roots({}..{})::", t.hex(), head.id().hex()),
                        "landing by rebase",
                    )?,
                    _ => {}
                }
                if let Some(c) = carry {
                    let c = commit(c)?;
                    if c.parent_ids() != [head.id().clone()] {
                        return Err(Error(format!(
                            "{} is not a child of the head that lands",
                            short(&c.id().hex())
                        )));
                    }
                }
                if let Some(b) = delete_bookmark {
                    name_ok(b)?;
                    if b == trunk {
                        return Err(Error("cannot delete the trunk bookmark it lands on".into()));
                    }
                }
            }
            GraphOp::Rebase { source, mode, dest } => {
                let s = commit(source)?;
                if dest.is_empty() {
                    return Err(Error("rebase needs a destination".into()));
                }
                let mut ds = Vec::new();
                for d in dest {
                    ds.push(commit(d)?.id().hex());
                }
                let dests = ds.join(" | ");
                let moved = match mode {
                    RebaseMode::Revision => s.id().hex(),
                    RebaseMode::Source => format!("{}::", s.id().hex()),
                    RebaseMode::Branch => format!("roots(({dests})..{})::", s.id().hex()),
                };
                // -r also rebases the descendants (onto the commit's parents).
                let rewritten = format!("{}::", s.id().hex());
                self.must_be_mutable(
                    repo,
                    if *mode == RebaseMode::Branch {
                        &moved
                    } else {
                        &rewritten
                    },
                    "this rebase",
                )?;
                let cycle = match mode {
                    RebaseMode::Revision => format!("({dests}) & {}", s.id().hex()),
                    _ => format!("({dests}) & ({moved})"),
                };
                if !self.eval_in(repo, &cycle)?.is_empty() {
                    return Err(Error(
                        "the destination is one of the commits being moved".into(),
                    ));
                }
            }
            GraphOp::Describe { commit: c, .. } => {
                let c = commit(c)?;
                self.must_be_mutable(repo, &c.id().hex(), "describe")?;
            }
            GraphOp::Abandon { commits } => {
                if commits.is_empty() {
                    return Err(Error("nothing to abandon".into()));
                }
                let mut ids = Vec::new();
                for c in commits {
                    ids.push(commit(c)?.id().hex());
                }
                self.must_be_mutable(repo, &format!("({})::", ids.join(" | ")), "abandon")?;
            }
            GraphOp::New { parents, .. } => {
                if parents.is_empty() {
                    return Err(Error("a new change needs a parent".into()));
                }
                for p in parents {
                    commit(p)?;
                }
            }
            GraphOp::Squash {
                source,
                into,
                message,
            } => {
                let (src, dest, _) =
                    self.squash_parts(repo, source, into.as_deref(), message.as_deref())?;
                if src.id() == repo.store().root_commit_id() {
                    return Err(Error("the root commit cannot be squashed".into()));
                }
                if src.id() == dest.id() {
                    return Err(Error("a change cannot be squashed into itself".into()));
                }
                if dest.id() == repo.store().root_commit_id() {
                    return Err(Error("nothing can be squashed into the root commit".into()));
                }
                self.must_be_mutable(
                    repo,
                    &format!("{} | {}", src.id().hex(), dest.id().hex()),
                    "this squash",
                )?;
            }
            GraphOp::Commit {
                workspace,
                message,
                bookmark,
            } => {
                let wc = repo
                    .view()
                    .get_wc_commit_id(&WorkspaceNameBuf::from(workspace.as_str()))
                    .cloned()
                    .ok_or_else(|| Error(format!("no workspace named {workspace}")))?;
                if message.trim().is_empty() {
                    return Err(Error("a commit needs a message".into()));
                }
                self.must_be_mutable(repo, &wc.hex(), "commit")?;
                if let Some(b) = bookmark {
                    name_ok(b)?;
                    let current = repo.view().get_local_bookmark(&ref_name(b));
                    if current.has_conflict() {
                        return Err(Error(format!("bookmark {b} is conflicted; set it first")));
                    }
                    if let Some(old) = current.as_normal()
                        && !self.is_ancestor(repo, old, &wc)?
                    {
                        return Err(Error(format!(
                            "{b} is not an ancestor of {workspace}@: moving it there goes backwards or sideways; commit, then move it with Bookmark…"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// The jj-lib run of a graph operation inside `tx`; returns the commit it created, if any.
    /// Descendants are rebased by the caller ([`Self::run_plan`]).
    pub(super) fn run_graph(&self, tx: &mut Transaction, op: &GraphOp) -> Result<Option<Commit>> {
        let get = |tx: &Transaction, hex: &str| -> Result<Commit> {
            tx.repo()
                .store()
                .get_commit(&parse_commit_id(hex)?)
                .map_err(err)
        };
        match op {
            GraphOp::SetBookmark { name, to, .. } => {
                let to = get(tx, to)?;
                tx.repo_mut()
                    .set_local_bookmark_target(&ref_name(name), RefTarget::normal(to.id().clone()));
                Ok(None)
            }
            GraphOp::DeleteBookmark { name } => {
                tx.repo_mut()
                    .set_local_bookmark_target(&ref_name(name), RefTarget::absent());
                Ok(None)
            }
            GraphOp::Land {
                head,
                trunk,
                strategy,
                message,
                carry,
                delete_bookmark,
            } => {
                let head = get(tx, head)?;
                let t = self.bookmark_target(tx.repo(), trunk)?;
                let ff = self.is_ancestor(tx.repo(), &t, head.id())?;
                let mut created = None;
                let new_trunk = if ff {
                    head.id().clone()
                } else {
                    match strategy {
                        LandStrategy::FastForward => {
                            return Err(Error(format!("{trunk} cannot fast-forward")));
                        }
                        LandStrategy::Rebase => {
                            let roots = self.eval_in(
                                tx.repo(),
                                &format!("roots({}..{})", t.hex(), head.id().hex()),
                            )?;
                            let stats = block(move_commits(
                                tx.repo_mut(),
                                &MoveCommitsLocation {
                                    new_parent_ids: vec![t.clone()],
                                    new_child_ids: vec![],
                                    target: MoveCommitsTarget::Roots(roots),
                                },
                                &RebaseOptions::default(),
                            ))
                            .map_err(err)?;
                            block(tx.repo_mut().rebase_descendants()).map_err(err)?;
                            match stats.rebased_commits.get(head.id()) {
                                Some(jj_lib::rewrite::RebasedCommit::Rewritten(c)) => {
                                    c.id().clone()
                                }
                                // Already in place, or abandoned (it became empty on the trunk).
                                _ => self
                                    .eval_in(tx.repo(), &head.change_id().reverse_hex())?
                                    .into_iter()
                                    .next()
                                    .unwrap_or_else(|| t.clone()),
                            }
                        }
                        LandStrategy::Merge => {
                            let parents = vec![get(tx, &t.hex())?, head.clone()];
                            let tree =
                                block(merge_commit_trees(tx.repo(), &parents)).map_err(err)?;
                            let m = message
                                .as_deref()
                                .filter(|m| !m.trim().is_empty())
                                .map(with_newline)
                                .unwrap_or_else(|| merge_message(trunk, &head));
                            let merge = block(
                                tx.repo_mut()
                                    .new_commit(vec![t.clone(), head.id().clone()], tree)
                                    .set_description(m)
                                    .write(),
                            )
                            .map_err(err)?;
                            if let Some(c) = carry {
                                let c = get(tx, c)?;
                                block(rebase_commit(tx.repo_mut(), c, vec![merge.id().clone()]))
                                    .map_err(err)?;
                                block(tx.repo_mut().rebase_descendants()).map_err(err)?;
                            }
                            let id = merge.id().clone();
                            created = Some(merge);
                            id
                        }
                    }
                };
                tx.repo_mut()
                    .set_local_bookmark_target(&ref_name(trunk), RefTarget::normal(new_trunk));
                if let Some(b) = delete_bookmark {
                    tx.repo_mut()
                        .set_local_bookmark_target(&ref_name(b), RefTarget::absent());
                }
                Ok(created)
            }
            GraphOp::Rebase { source, mode, dest } => {
                let s = get(tx, source)?;
                let dest_ids: Vec<JjCommitId> = dest
                    .iter()
                    .map(|d| parse_commit_id(d))
                    .collect::<Result<_>>()?;
                let target = match mode {
                    RebaseMode::Revision => MoveCommitsTarget::Commits(vec![s.id().clone()]),
                    RebaseMode::Source => MoveCommitsTarget::Roots(vec![s.id().clone()]),
                    RebaseMode::Branch => {
                        let dests: Vec<String> = dest_ids.iter().map(|d| d.hex()).collect();
                        MoveCommitsTarget::Roots(self.eval_in(
                            tx.repo(),
                            &format!("roots(({})..{})", dests.join(" | "), s.id().hex()),
                        )?)
                    }
                };
                block(move_commits(
                    tx.repo_mut(),
                    &MoveCommitsLocation {
                        new_parent_ids: dest_ids,
                        new_child_ids: vec![],
                        target,
                    },
                    &RebaseOptions::default(),
                ))
                .map_err(err)?;
                Ok(None)
            }
            GraphOp::Describe { commit, message } => {
                let c = get(tx, commit)?;
                let m = if message.trim().is_empty() {
                    String::new()
                } else {
                    with_newline(message)
                };
                block(tx.repo_mut().rewrite_commit(&c).set_description(m).write()).map_err(err)?;
                Ok(None)
            }
            GraphOp::Abandon { commits } => {
                // Unique, children before parents does not matter: each is recorded, then one
                // rebase moves every descendant onto the nearest kept ancestors.
                let mut seen = BTreeSet::new();
                for hex in commits {
                    if seen.insert(hex.clone()) {
                        let c = get(tx, hex)?;
                        tx.repo_mut().record_abandoned_commit(&c);
                    }
                }
                Ok(None)
            }
            GraphOp::New { parents, message } => {
                let ps: Vec<Commit> = parents.iter().map(|p| get(tx, p)).collect::<Result<_>>()?;
                let tree = block(merge_commit_trees(tx.repo(), &ps)).map_err(err)?;
                let mut b = tx
                    .repo_mut()
                    .new_commit(ps.iter().map(|p| p.id().clone()).collect(), tree);
                if let Some(m) = message.as_deref().filter(|m| !m.trim().is_empty()) {
                    b = b.set_description(with_newline(m));
                }
                Ok(Some(block(b.write()).map_err(err)?))
            }
            GraphOp::Commit {
                workspace,
                message,
                bookmark,
            } => {
                // As `jj commit`: describe @, then a new empty @ on top for every workspace there.
                let ws = WorkspaceNameBuf::from(workspace.as_str());
                let wc_id = tx
                    .repo()
                    .view()
                    .get_wc_commit_id(&ws)
                    .cloned()
                    .ok_or_else(|| Error(format!("no workspace named {workspace}")))?;
                let wc = get(tx, &wc_id.hex())?;
                let names: Vec<WorkspaceNameBuf> = tx
                    .repo()
                    .view()
                    .wc_commit_ids()
                    .iter()
                    .filter(|(_, id)| **id == wc_id)
                    .map(|(n, _)| n.clone())
                    .collect();
                let committed = block(
                    tx.repo_mut()
                        .rewrite_commit(&wc)
                        .set_description(with_newline(message))
                        .write(),
                )
                .map_err(err)?;
                let next = block(
                    tx.repo_mut()
                        .new_commit(vec![committed.id().clone()], committed.tree())
                        .write(),
                )
                .map_err(err)?;
                for n in names {
                    block(tx.repo_mut().edit(n, &next)).map_err(err)?;
                }
                if let Some(b) = bookmark {
                    tx.repo_mut().set_local_bookmark_target(
                        &ref_name(b),
                        RefTarget::normal(committed.id().clone()),
                    );
                }
                Ok(Some(committed))
            }
            GraphOp::Squash {
                source,
                into,
                message,
            } => {
                let (src, dest, m) =
                    self.squash_parts(tx.repo(), source, into.as_deref(), message.as_deref())?;
                let sel = CommitWithSelection {
                    parent_tree: block(src.parent_tree(tx.repo())).map_err(err)?,
                    selected_tree: src.tree(),
                    commit: src,
                };
                if let Some(sq) =
                    block(squash_commits(tx.repo_mut(), &[sel], &dest, false)).map_err(err)?
                {
                    block(sq.commit_builder.set_description(m).write()).map_err(err)?;
                }
                Ok(None)
            }
        }
    }
}

/// Local bookmarks whose target differs between two repo states: (name, before, after).
pub(super) fn moved_bookmarks(
    before: &dyn Repo,
    after: &dyn Repo,
) -> Vec<(String, Option<String>, Option<String>)> {
    let map = |r: &dyn Repo| -> BTreeMap<String, Vec<String>> {
        r.view()
            .local_bookmarks()
            .map(|(n, t)| {
                (
                    n.as_str().to_string(),
                    t.added_ids().map(|i| i.hex()).collect(),
                )
            })
            .collect()
    };
    let (a, b) = (map(before), map(after));
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    names
        .into_iter()
        .filter(|n| a.get(*n) != b.get(*n))
        .map(|n| {
            let one = |m: &BTreeMap<String, Vec<String>>| m.get(n).and_then(|v| v.first().cloned());
            (n.clone(), one(&a), one(&b))
        })
        .collect()
}

/// The plan as a graph operation, if it is one.
pub(super) fn graph_op(plan: &MutationPlan) -> Option<&GraphOp> {
    match plan {
        MutationPlan::Graph(op) => Some(op),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::bookmark_name_ok;

    #[test]
    fn bookmark_names() {
        for ok in ["main", "feature/x", "v1.2", "fix-1", "a+b", "ünï"] {
            assert!(bookmark_name_ok(ok), "{ok}");
        }
        for bad in ["", "-x", "a b", "x@origin", "glob:*", "a..b", "a/", "x\n"] {
            assert!(!bookmark_name_ok(bad), "{bad:?}");
        }
    }
}
