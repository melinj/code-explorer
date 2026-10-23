//! The jj CLI fallback of [`JjWriter::mutate`] (DESIGN D4, §10; N11).
//!
//! Each [`MutationPlan`] has a CLI recipe built from the commands verified in DESIGN appendix A.
//! File contents never go through a diff editor: they are written into a temporary workspace
//! (`jj workspace add -r X`, a child of X) and moved with `jj restore` / `jj absorb`, so no tool
//! program is needed except `cp` as the merge tool of `jj resolve`. Rules:
//! - argv only: every command is a [`Command`] with its arguments, never a shell;
//! - a server-owned `--config-file`, written to a private (0700) temp directory per run: user
//!   name and email, no pager, no editor, the `cv-resolve` merge tool; the writer's extra TOML
//!   rides along as a second file, and an isolated writer hides the user's config (`JJ_CONFIG`);
//! - the caller holds the mutation queue; other workspaces are snapshotted before and brought
//!   up to date (`jj workspace update-stale`) after, like the jj-lib path;
//! - every command is logged to stderr.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use jj_lib::object_id::ObjectId;
use jj_lib::repo::Repo;

use super::{
    EditMode, FileWrite, GraphOp, JjWriter, LandStrategy, MutationOutcome, MutationPlan,
    RebaseMode, WorkspaceEffect, parse_commit_id, toml_str,
};
use crate::{Error, Result, err};

static RUN: AtomicU64 = AtomicU64::new(0);

/// The `jj` binary (`CV_JJ` overrides; it must be the version jj-lib is pinned to).
fn jj_bin() -> String {
    std::env::var("CV_JJ").unwrap_or_else(|_| "jj".into())
}

pub(super) fn plan_name(plan: &MutationPlan) -> &'static str {
    match plan {
        MutationPlan::WriteFiles { mode, .. } => match mode {
            EditMode::Amend => "write (amend)",
            EditMode::InsertAfter => "write (insert-after)",
            EditMode::Branch => "write (branch)",
            EditMode::KeepDescendants => "write (keep-descendants)",
        },
        MutationPlan::Absorb { .. } => "absorb",
        MutationPlan::Resolve { .. } => "resolve",
        MutationPlan::Revert { .. } => "revert",
        MutationPlan::Restore { .. } => "restore",
        MutationPlan::Graph(op) => op.name(),
    }
}

/// A fileset naming exactly these paths (`root-file:"…"`, quoted, so no pattern syntax applies).
fn fileset(paths: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    paths
        .into_iter()
        .map(|p| format!("root-file:{}", toml_str(p.as_ref())))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// One fallback run: its private directory, config files and temporary workspace.
pub(super) struct Run<'a> {
    w: &'a JjWriter,
    dir: PathBuf,
    configs: Vec<PathBuf>,
    /// Isolated writers: `JJ_CONFIG` points here (an empty file) so the user's config is skipped.
    empty: PathBuf,
    /// The temporary workspace (name, root) while it exists.
    ws: Option<(String, PathBuf)>,
}

impl<'a> Run<'a> {
    pub(super) fn new(w: &'a JjWriter, resolve_content: Option<&[u8]>) -> Result<Self> {
        let n = RUN.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cv-jj-cli-{}-{n}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        {
            let mut b = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            b.create(&dir).map_err(err)?;
        }
        let mut run = Run {
            w,
            empty: dir.join("empty.toml"),
            configs: Vec::new(),
            dir,
            ws: None,
        };
        std::fs::write(&run.empty, "").map_err(err)?;
        if let Some(extra) = &w.extra {
            let p = run.dir.join("extra.toml");
            std::fs::write(&p, extra).map_err(err)?;
            run.configs.push(p);
        }
        let content = run.dir.join("resolve-content");
        std::fs::write(&content, resolve_content.unwrap_or_default()).map_err(err)?;
        let cfg = format!(
            "[user]\nname = {}\nemail = {}\n\n[ui]\npaginate = \"never\"\ncolor = \"never\"\neditor = \"true\"\n\n\
             [merge-tools.cv-resolve]\nprogram = \"cp\"\nmerge-args = [{}, \"$output\"]\n\
             merge-tool-edits-conflict-markers = true\n",
            toml_str(w.settings.user_name()),
            toml_str(w.settings.user_email()),
            toml_str(&content.to_string_lossy()),
        );
        let p = run.dir.join("cv.toml");
        std::fs::write(&p, cfg).map_err(err)?;
        run.configs.push(p);
        Ok(run)
    }

    /// `jj <args>` in `cwd` with the server's config files, argv only; logged unless `quiet`.
    fn command(&self, cwd: &Path, args: &[&str], quiet: bool) -> Command {
        let mut cmd = Command::new(jj_bin());
        for c in &self.configs {
            cmd.arg("--config-file").arg(c);
        }
        cmd.arg("--no-pager").args(args);
        cmd.current_dir(cwd)
            .stdin(Stdio::null())
            .env_remove("JJ_EDITOR")
            .env_remove("JJ_PAGER");
        if self.w.isolated {
            cmd.env("JJ_CONFIG", &self.empty);
        }
        if !quiet {
            eprintln!(
                "cv-vcs: jj CLI (in {}): jj {}",
                cwd.display(),
                args.join(" ")
            );
        }
        cmd
    }

    /// A network command (`jj git fetch | push`): never interactive (no controlling terminal, so
    /// ssh cannot ask for a passphrase there; no git credential prompts) and killed with its
    /// children after `timeout`. What jj printed (it reports on stderr) on success.
    pub(super) fn jj_net(
        &self,
        cwd: &Path,
        args: &[&str],
        timeout: std::time::Duration,
    ) -> Result<String> {
        let mut cmd = self.command(cwd, args, false);
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe; the child gets its own session and group.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        let what = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        let mut child = cmd
            .spawn()
            .map_err(|e| Error(format!("cannot run {}: {e}", jj_bin())))?;
        let read = |p: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut s = String::new();
                if let Some(mut p) = p {
                    let _ = p.read_to_string(&mut s);
                }
                s
            })
        };
        let out = read(child.stdout.take().map(|p| Box::new(p) as _));
        let errs = read(child.stderr.take().map(|p| Box::new(p) as _));
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(st) = child.try_wait().map_err(err)? {
                break st;
            }
            if start.elapsed() > timeout {
                #[cfg(unix)]
                // SAFETY: signals the child's own process group (setsid above).
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error(format!(
                    "jj {what} gave no answer in {} s (a remote asking for credentials?)",
                    timeout.as_secs()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let (out, errs) = (
            out.join().unwrap_or_default(),
            errs.join().unwrap_or_default(),
        );
        if !status.success() {
            return Err(Error(format!("jj {what}: {}", errs.trim())));
        }
        Ok(format!("{}{}", out, errs).trim().to_string())
    }

    /// `jj <args>` for a read that must not snapshot or log (`--ignore-working-copy`).
    pub(super) fn jj_quiet(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let mut all = vec!["--ignore-working-copy"];
        all.extend_from_slice(args);
        self.output(self.command(cwd, &all, true), args)
    }

    /// `jj <args>` in `cwd`, argv only; stdout on success.
    fn jj(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        self.output(self.command(cwd, args, false), args)
    }

    fn output(&self, mut cmd: Command, args: &[&str]) -> Result<String> {
        let out = cmd
            .output()
            .map_err(|e| Error(format!("cannot run {}: {e}", jj_bin())))?;
        if !out.status.success() {
            return Err(Error(format!(
                "jj {}: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn root(&self) -> &Path {
        &self.w.root
    }

    /// A temporary workspace whose `@` is a new child of `rev`, with `files` written on disk and
    /// snapshotted. Returns the workspace's `@` symbol (`name@`).
    fn workspace_with(&mut self, rev: &str, files: &[FileWrite]) -> Result<String> {
        let name = format!(
            "cv-fallback-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        );
        let path = self.dir.join("ws");
        let path_s = path.to_string_lossy().into_owned();
        self.jj(
            self.root(),
            &["workspace", "add", "--name", &name, "-r", rev, &path_s],
        )?;
        self.ws = Some((name.clone(), path.clone()));
        for f in files {
            // The checked path, and no symlink on the way: a link checked out from the commit
            // must not carry the write out of the temporary workspace.
            if !super::tree_path_ok(&f.path) {
                return Err(Error(format!("invalid repository path {:?}", f.path)));
            }
            let p = path.join(&f.path);
            let mut at = path.clone();
            let parts: Vec<&str> = f.path.split('/').collect();
            for (i, c) in parts.iter().enumerate() {
                at.push(c);
                // (Deleting a symlink itself is fine: remove_file removes the link.)
                let last_delete = i + 1 == parts.len() && f.content.is_none();
                if !last_delete
                    && std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink())
                {
                    return Err(Error(format!("{} is a symlink in the tree", f.path)));
                }
            }
            match &f.content {
                Some(bytes) => {
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent).map_err(err)?;
                    }
                    std::fs::write(&p, bytes).map_err(err)?;
                }
                None => match std::fs::remove_file(&p) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(err(e)),
                    _ => {}
                },
            }
        }
        // Any command run in the workspace snapshots its files into its `@`.
        self.jj(&path, &["status"])?;
        Ok(format!("{name}@"))
    }

    /// Change ids of the commits of `revset`, one per line of `jj log`.
    fn changes_of(&self, revset: &str) -> Result<Vec<String>> {
        Ok(self
            .jj(
                self.root(),
                &[
                    "log",
                    "--no-graph",
                    "-r",
                    revset,
                    "-T",
                    "change_id ++ \"\\n\"",
                ],
            )?
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// `jj new --no-edit <parents> [-m <message>]`, returning the new change (the child of the
    /// first parent that was not there before).
    fn new_no_edit(&self, parents: &[String], message: Option<&str>) -> Result<String> {
        let first = parents.first().ok_or_else(|| Error("no parents".into()))?;
        let before = self.changes_of(&format!("children({first})"))?;
        let mut args = vec!["new", "--no-edit"];
        args.extend(parents.iter().map(String::as_str));
        if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
            args.extend(["-m", m]);
        }
        self.jj(self.root(), &args)?;
        self.changes_of(&format!("children({first})"))?
            .into_iter()
            .find(|c| !before.contains(c))
            .ok_or_else(|| Error("jj new: the new change was not found".into()))
    }

    fn change_of(&self, rev: &str) -> Result<String> {
        Ok(self
            .jj(
                self.root(),
                &["log", "--no-graph", "-r", rev, "-T", "change_id ++ \"\\n\""],
            )?
            .trim()
            .to_string())
    }

    /// Forget the temporary workspace (jj abandons its `@` when it is empty) and delete it.
    fn drop_workspace(&mut self) -> Result<()> {
        if let Some((name, path)) = self.ws.take() {
            let r = self.jj(self.root(), &["workspace", "forget", &name]);
            let _ = std::fs::remove_dir_all(&path);
            r?;
        }
        Ok(())
    }
}

impl Drop for Run<'_> {
    fn drop(&mut self) {
        let _ = self.drop_workspace();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl JjWriter {
    /// Replays `plan` through the jj CLI. The caller holds the mutation queue and validated the
    /// plan.
    pub(super) fn mutate_cli(&self, plan: &MutationPlan) -> Result<MutationOutcome> {
        let op_before = self.head()?.op_id().hex();
        let before_ws = self.workspaces()?;
        let resolve_content = match plan {
            MutationPlan::Resolve { content, .. } => Some(content.as_slice()),
            _ => None,
        };
        let mut run = Run::new(self, resolve_content)?;
        // Snapshot every workspace first (the jj-lib path snapshots the affected ones), so the
        // recipe sees their files and none of them goes stale with edits on disk.
        for w in &before_ws {
            if let Some(root) = w.root.as_ref().filter(|r| r.join(".jj").is_dir())
                && let Err(e) = run.jj(root, &["status"])
            {
                eprintln!(
                    "cv-vcs: jj CLI: snapshot of workspace {} failed: {e}",
                    w.name
                );
            }
        }
        *self.cache.lock().unwrap() = None;
        let snap = self.head()?;
        // Commits are addressed by change id: the snapshots may have rewritten them.
        let change = |hex: &str| -> Result<String> {
            Ok(snap
                .store()
                .get_commit(&parse_commit_id(hex)?)
                .map_err(err)?
                .change_id()
                .reverse_hex())
        };
        let mut created: Option<String> = None;
        match plan {
            MutationPlan::WriteFiles {
                target,
                files,
                mode,
                message,
            } => {
                let target = change(target)?;
                let at = run.workspace_with(&target, files)?;
                match mode {
                    EditMode::Amend | EditMode::KeepDescendants => {
                        let paths = fileset(files.iter().map(|f| f.path.as_str()));
                        let mut args = vec!["restore", "--from", &at, "--into", &target];
                        if *mode == EditMode::KeepDescendants {
                            args.push("--restore-descendants");
                        }
                        args.extend(["--", paths.as_str()]);
                        run.jj(run.root(), &args)?;
                    }
                    EditMode::Branch | EditMode::InsertAfter => {
                        if let Some(m) = message.as_deref().filter(|m| !m.trim().is_empty()) {
                            run.jj(run.root(), &["describe", &at, "-m", m])?;
                        }
                        let new = run.change_of(&at)?;
                        if *mode == EditMode::InsertAfter {
                            run.jj(
                                run.root(),
                                &["rebase", "-r", &new, "--insert-after", &target],
                            )?;
                        }
                        created = Some(new);
                    }
                }
            }
            MutationPlan::Absorb { tip, files, into } => {
                let tip = change(tip)?;
                let at = run.workspace_with(&tip, files)?;
                let dest = format!("({into}) & mutable() & ::{tip}");
                run.jj(run.root(), &["absorb", "--from", &at, "--into", &dest])?;
                // Leftovers (hunks no commit owns) amend the tip, like the jj-lib path.
                run.jj(
                    run.root(),
                    &[
                        "squash",
                        "--from",
                        &at,
                        "--into",
                        &tip,
                        "--use-destination-message",
                    ],
                )?;
            }
            MutationPlan::Resolve { commit, path, .. } => {
                let c = change(commit)?;
                run.jj(
                    run.root(),
                    &[
                        "resolve",
                        "-r",
                        &c,
                        "--tool",
                        "cv-resolve",
                        "--",
                        &fileset([path]),
                    ],
                )?;
            }
            MutationPlan::Revert { op } => {
                run.jj(run.root(), &["op", "revert", op])?;
            }
            MutationPlan::Restore { op } => {
                run.jj(run.root(), &["op", "restore", op])?;
            }
            MutationPlan::Graph(op) => created = graph_recipe(&run, op, &change)?,
        }
        run.drop_workspace()?;
        // Bring every other workspace to its new `@` (the recipe ran in this one or in the
        // temporary one, which jj updates itself).
        let roots: Vec<(String, PathBuf)> = before_ws
            .iter()
            .filter_map(|w| Some((w.name.clone(), w.root.clone()?)))
            .filter(|(_, r)| r.join(".jj").is_dir())
            .collect();
        let mut updated = std::collections::BTreeMap::new();
        for (name, root) in &roots {
            let ok = run.jj(root, &["workspace", "update-stale"]).is_ok();
            updated.insert(name.clone(), ok);
        }
        drop(run);
        *self.cache.lock().unwrap() = None;
        let after = self.head()?;
        let tx = after.start_transaction();
        let created_commit = match &created {
            Some(ch) => {
                let id = self
                    .eval_in(tx.repo(), ch)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| Error(format!("created change {ch} not found")))?;
                Some(tx.repo().store().get_commit(&id).map_err(err)?)
            }
            None => None,
        };
        let mut out = self.summarize(&snap, tx.repo(), created_commit)?;
        out.op_before = op_before;
        out.op_after = Some(after.op_id().hex());
        out.workspaces = self
            .changed_workspaces(&snap, tx.repo())
            .into_iter()
            .map(|name| WorkspaceEffect {
                updated: updated.get(&name).copied().unwrap_or(false),
                name,
                snapshotted: true,
                recovered_into: None,
            })
            .collect();
        Ok(out)
    }
}

/// The CLI recipe of a graph operation; returns the change it created, if any. Commits are named
/// by change id (`change`), since the snapshots before the recipe may have rewritten them.
fn graph_recipe(
    run: &Run<'_>,
    op: &GraphOp,
    change: &dyn Fn(&str) -> Result<String>,
) -> Result<Option<String>> {
    let root = run.root();
    match op {
        GraphOp::SetBookmark {
            name,
            to,
            allow_backwards,
        } => {
            let to = change(to)?;
            let mut args = vec!["bookmark", "set", name.as_str(), "-r", to.as_str()];
            if *allow_backwards {
                args.push("--allow-backwards");
            }
            run.jj(root, &args)?;
        }
        GraphOp::DeleteBookmark { name } => {
            run.jj(
                root,
                &["bookmark", "delete", &format!("exact:{}", toml_str(name))],
            )?;
        }
        GraphOp::Land {
            head,
            trunk,
            strategy,
            message,
            carry,
            delete_bookmark,
        } => {
            let head = change(head)?;
            let ff = !run.changes_of(&format!("{trunk} & ::{head}"))?.is_empty();
            let mut created = None;
            let target = if ff {
                head
            } else {
                match strategy {
                    LandStrategy::FastForward => {
                        return Err(Error(format!("{trunk} cannot fast-forward")));
                    }
                    LandStrategy::Rebase => {
                        run.jj(root, &["rebase", "-b", &head, "-d", trunk])?;
                        head
                    }
                    LandStrategy::Merge => {
                        let m = run.new_no_edit(
                            &[trunk.clone(), head.clone()],
                            Some(
                                message
                                    .as_deref()
                                    .filter(|m| !m.trim().is_empty())
                                    .unwrap_or(&format!(
                                        "Merge {} into {trunk}",
                                        &head[..12.min(head.len())]
                                    )),
                            ),
                        )?;
                        if let Some(c) = carry {
                            let c = change(c)?;
                            run.jj(root, &["rebase", "-r", &c, "-d", &m])?;
                        }
                        created = Some(m.clone());
                        m
                    }
                }
            };
            run.jj(root, &["bookmark", "set", trunk, "-r", &target])?;
            if let Some(b) = delete_bookmark {
                run.jj(
                    root,
                    &["bookmark", "delete", &format!("exact:{}", toml_str(b))],
                )?;
            }
            return Ok(created);
        }
        GraphOp::Rebase { source, mode, dest } => {
            let s = change(source)?;
            let flag = match mode {
                RebaseMode::Revision => "-r",
                RebaseMode::Source => "-s",
                RebaseMode::Branch => "-b",
            };
            let dests: Vec<String> = dest.iter().map(|d| change(d)).collect::<Result<_>>()?;
            let mut args = vec!["rebase", flag, s.as_str()];
            for d in &dests {
                args.extend(["-d", d.as_str()]);
            }
            run.jj(root, &args)?;
        }
        GraphOp::Describe { commit, message } => {
            let c = change(commit)?;
            run.jj(root, &["describe", &c, "-m", message])?;
        }
        GraphOp::Abandon { commits } => {
            let cs: Vec<String> = commits.iter().map(|c| change(c)).collect::<Result<_>>()?;
            let mut args = vec!["abandon"];
            args.extend(cs.iter().map(String::as_str));
            run.jj(root, &args)?;
        }
        GraphOp::New { parents, message } => {
            let ps: Vec<String> = parents.iter().map(|p| change(p)).collect::<Result<_>>()?;
            return Ok(Some(run.new_no_edit(&ps, message.as_deref())?));
        }
        GraphOp::Squash {
            source,
            into,
            message,
        } => {
            // The description is decided here, as jj-lib does it (jj's would open an editor).
            let repo = run.w.head()?;
            let (src, dest, m) =
                run.w
                    .squash_parts(repo.as_ref(), source, into.as_deref(), message.as_deref())?;
            let (s, d) = (change(&src.id().hex())?, change(&dest.id().hex())?);
            run.jj(
                root,
                &["squash", "--from", &s, "--into", &d, "-m", m.trim_end()],
            )?;
        }
        GraphOp::Commit {
            workspace,
            message,
            bookmark,
        } => {
            // `jj commit` runs in the workspace; the committed change keeps @'s change id.
            let w = run
                .w
                .workspaces()?
                .into_iter()
                .find(|w| w.name == *workspace)
                .ok_or_else(|| Error(format!("no workspace named {workspace}")))?;
            let ws_root = w
                .root
                .filter(|r| r.join(".jj").is_dir())
                .ok_or_else(|| Error(format!("workspace {workspace} has no files here")))?;
            run.jj(&ws_root, &["commit", "-m", message])?;
            if let Some(b) = bookmark {
                run.jj(root, &["bookmark", "set", b, "-r", &w.change])?;
            }
            return Ok(Some(w.change));
        }
    }
    Ok(None)
}
