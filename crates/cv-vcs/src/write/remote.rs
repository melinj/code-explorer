//! `jj git fetch` and `jj git push` for the Graph view. They run through the jj CLI, not jj-lib,
//! so the user's own git setup applies exactly as in a terminal (remotes, credential helpers, the
//! ssh agent, push safety checks), inside the mutation queue. They are never interactive (see
//! `Run::jj_net`): a remote that wants a password fails instead of waiting.

use std::collections::BTreeMap;
use std::time::Duration;

use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;

use super::cli::Run;
use super::graph::bookmark_name_ok;
use super::{JjWriter, toml_str};
use crate::{Error, Result};

/// How long one fetch or push may take.
const NET_TIMEOUT: Duration = Duration::from_secs(180);

/// A local bookmark (`name`) or remote-tracking bookmark (`name@remote`) a fetch or push changed.
#[derive(Debug, Clone, PartialEq)]
pub struct RefMove {
    pub name: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteOutcome {
    pub op_before: String,
    pub op_after: String,
    /// What jj printed, trimmed (the push plan for a dry run).
    pub output: String,
    pub moved: Vec<RefMove>,
}

/// Remote names cv passes to jj: what `git remote add` accepts, never an option.
fn remote_name_ok(r: &str) -> bool {
    !r.is_empty()
        && !r.starts_with('-')
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

impl JjWriter {
    /// The git remotes of the repo, sorted (none when it is not git-backed). Asked of the jj CLI:
    /// jj-lib keeps the git config it read when the repo was opened, so a remote added since would
    /// be missing.
    pub fn git_remotes(&self) -> Result<Vec<String>> {
        let repo = self.head()?;
        if jj_lib::git::get_git_backend(repo.store()).is_err() {
            return Ok(vec![]);
        }
        let run = Run::new(self, None)?;
        let mut names: Vec<String> = run
            .jj_quiet(&self.root, &["git", "remote", "list"])?
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .collect();
        names.sort();
        Ok(names)
    }

    fn check_remote(&self, remote: &str) -> Result<()> {
        if !remote_name_ok(remote) || !self.git_remotes()?.iter().any(|r| r == remote) {
            return Err(Error(format!("no git remote named {remote:?}")));
        }
        Ok(())
    }

    /// `jj git fetch [--remote <remote>]` (jj's default remotes otherwise).
    pub fn git_fetch(&self, remote: Option<&str>) -> Result<RemoteOutcome> {
        if self.git_remotes()?.is_empty() {
            return Err(Error(
                "this repository has no git remote to fetch from".into(),
            ));
        }
        let mut args = vec!["git", "fetch"];
        if let Some(r) = remote {
            self.check_remote(r)?;
            args.extend(["--remote", r]);
        }
        self.remote_run(&args)
    }

    /// `jj git push -b <bookmark>… [--remote <remote>] [--dry-run]`. jj's own checks apply (no
    /// changes without a description or with conflicts, no force-push over remote changes it has
    /// not fetched).
    pub fn git_push(
        &self,
        bookmarks: &[String],
        remote: Option<&str>,
        dry_run: bool,
    ) -> Result<RemoteOutcome> {
        if bookmarks.is_empty() {
            return Err(Error("push needs at least one bookmark".into()));
        }
        let known = self.bookmarks()?;
        for b in bookmarks {
            if !bookmark_name_ok(b) || !known.iter().any(|k| k.name == *b) {
                return Err(Error(format!("no bookmark named {b:?}")));
            }
        }
        if self.git_remotes()?.is_empty() {
            return Err(Error("this repository has no git remote to push to".into()));
        }
        let patterns: Vec<String> = bookmarks
            .iter()
            .map(|b| format!("exact:{}", toml_str(b)))
            .collect();
        let mut args = vec!["git", "push"];
        if let Some(r) = remote {
            self.check_remote(r)?;
            args.extend(["--remote", r]);
        }
        for p in &patterns {
            args.extend(["-b", p.as_str()]);
        }
        if dry_run {
            args.push("--dry-run");
        }
        self.remote_run(&args)
    }

    fn remote_run(&self, args: &[&str]) -> Result<RemoteOutcome> {
        let _q = self.queue.lock().unwrap();
        let refs = |w: &JjWriter| -> Result<BTreeMap<String, Option<String>>> {
            let mut m = BTreeMap::new();
            for b in w.bookmarks()? {
                m.insert(b.name.clone(), b.targets.first().cloned());
                for r in b.remotes {
                    m.insert(
                        format!("{}@{}", b.name, r.remote),
                        r.targets.first().cloned(),
                    );
                }
            }
            Ok(m)
        };
        let before = refs(self)?;
        let op_before = self.head()?.op_id().hex();
        let run = Run::new(self, None)?;
        let out = run.jj_net(&self.root, args, NET_TIMEOUT);
        drop(run);
        *self.cache.lock().unwrap() = None;
        let output = out?;
        let after = refs(self)?;
        let mut moved = Vec::new();
        for name in before.keys().chain(after.keys()) {
            let (a, b) = (
                before.get(name).cloned().flatten(),
                after.get(name).cloned().flatten(),
            );
            if a != b && !moved.iter().any(|m: &RefMove| m.name == *name) {
                moved.push(RefMove {
                    name: name.clone(),
                    from: a,
                    to: b,
                });
            }
        }
        Ok(RemoteOutcome {
            op_before,
            op_after: self.head()?.op_id().hex(),
            output,
            moved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::remote_name_ok;

    #[test]
    fn remote_names() {
        for ok in ["origin", "upstream", "my-fork", "a.b_c"] {
            assert!(remote_name_ok(ok), "{ok}");
        }
        for bad in ["", "-o", "--upload-pack=x", "a b", "a/b", "x;y"] {
            assert!(!remote_name_ok(bad), "{bad:?}");
        }
    }
}
