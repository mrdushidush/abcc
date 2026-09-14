//! Isolation and checkpoints: a git worktree at a temp-index snapshot sha.
//!
//! ADR-0007, which answered three questions as one mechanism — what a checkpoint
//! *is*, how two attempts avoid each other, and what parallel isolation costs on
//! a 32 GB box.
//!
//! 🚨 **The marker is free and the working directory is not.** A worktree costs
//! **0.08–0.66 s and 0.8–63.7 MB** against **6–136 s and 1.7–25.5 GB** to copy the
//! same tree (F327). The difference is not the mechanism, it is *what git
//! ignores*: a worktree contains the repository and none of the toolchain.
//!
//! The full cycle, measured on four real repositories (F330): **take 0.16 s ·
//! diff 0.024 s · hand to an isolated attempt 0.25 s · restore 0.06 s.**
//!
//! # Three donor facts this module is shaped by
//!
//! * **No donor ever runs two agents against one tree**, and v1's parallel
//!   endpoint has no caller; its three file-locking surfaces all exclude the one
//!   agent that writes code (F326). Isolation has to be enforced where the path
//!   is resolved, and a worktree does exactly that.
//! * **`git status` clean is not a claim about content.** 241 of 539 tracked
//!   files in the v1 donor differ from their blobs while the tree reports clean,
//!   because `status` answers from a stat cache (F328). Nothing here asks it.
//! * **BCF's mechanism is kept and its wiring is not** (F325): it never calls
//!   `git worktree remove` or `prune` — neither string appears anywhere in that
//!   repository — so metadata accumulates. [`Worktree::close`] calls both.
//!
//! # The restore contract, stated honestly
//!
//! **Restore is exact _up to the repository's own text attributes_.** The round
//! trip normalises line endings the way `.gitattributes` says (219 CRLF in, 0
//! out, F330), and the repository with no such rule accumulated 241 divergent
//! files without noticing. 2.0 must never claim a byte-exactness it does not
//! have, and the honest claim is the one git itself makes.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where checkpoint refs live. 🚨 Writing the ref is **not optional**: an
/// unreferenced snapshot does not survive `git gc --prune=now` (F330), and this
/// is what makes `Holding { checkpoint }` and every fork-from-checkpoint promise
/// real rather than aspirational.
pub const CHECKPOINT_REFS: &str = "refs/abcc/checkpoints";

#[derive(Debug, thiserror::Error)]
pub enum VcsError {
    #[error("running `git {args}`: {source}")]
    Spawn {
        args: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`git {args}` failed with {code}: {stderr}")]
    Git {
        args: String,
        code: String,
        stderr: String,
    },
    #[error("{path} is not inside a git repository")]
    NotARepo { path: String },
    /// A scratch file this crate writes for git to read could not be placed. It
    /// is its own variant rather than an `Io` catch-all because there is exactly
    /// one thing it can be about, and a caller that sees it has a full disk or a
    /// read-only git directory rather than a repository problem.
    #[error("writing {path}: {source}")]
    Scratch {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("`git {args}` printed something this code cannot read: {got}")]
    Unreadable { args: String, got: String },
}

type Result<T> = std::result::Result<T, VcsError>;

/// A commit sha. Newtyped because a sha and a ref name and a path are all
/// `String` otherwise, and exactly one of them belongs in `git commit-tree -p`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha(String);

impl Sha {
    /// # Errors
    ///
    /// Fails if the text is not 40 hexadecimal characters.
    pub fn parse(raw: &str) -> std::result::Result<Sha, String> {
        let s = raw.trim();
        if s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Sha(s.to_owned()))
        } else {
            Err(format!("not a sha: {s:?}"))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first seven characters, for the console.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..7]
    }
}

impl fmt::Display for Sha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What changed between two snapshots.
///
/// This is the gate's **free structural rung**: 0.024 s, exactly the paths the
/// agent touched, with ignored build output excluded by construction — because
/// `.gitignore` is the repository's own answer to *is this a source file*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed { from: String },
    Copied { from: String },
    TypeChanged,
}

impl ChangeKind {
    fn from_status(code: &str, from: Option<&str>) -> ChangeKind {
        match code.chars().next() {
            Some('A') => ChangeKind::Added,
            Some('D') => ChangeKind::Deleted,
            Some('T') => ChangeKind::TypeChanged,
            Some('R') => ChangeKind::Renamed {
                from: from.unwrap_or_default().to_owned(),
            },
            Some('C') => ChangeKind::Copied {
                from: from.unwrap_or_default().to_owned(),
            },
            // 'M', and anything else git grows later: a change we can see but
            // cannot name is still a change, and calling it modified is the
            // conservative reading.
            _ => ChangeKind::Modified,
        }
    }
}

/// A repository the fleet works against.
#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Open the repository containing `path`.
    ///
    /// 🚨 **In a worktree `.git` is a file, not a directory** — this asks
    /// `git rev-parse` rather than looking for a directory, and treats "this is a
    /// worktree" as the normal case, because under ADR-0007 it *is* the normal
    /// case. Claudette met this and chose to degrade silently, landing its
    /// mission marker in the PR instead of `.git/info/exclude`.
    ///
    /// # Errors
    ///
    /// Fails if `path` is not inside a git repository.
    pub fn open(path: &Path) -> Result<Repo> {
        let out = run(path, ["rev-parse", "--show-toplevel"]).map_err(|e| match e {
            VcsError::Git { .. } => VcsError::NotARepo {
                path: path.display().to_string(),
            },
            other => other,
        })?;
        Ok(Repo {
            root: PathBuf::from(out.trim()),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The common git directory — the real one, shared by every worktree.
    ///
    /// # Errors
    ///
    /// Fails if git will not answer.
    pub fn common_dir(&self) -> Result<PathBuf> {
        let out = run(&self.root, ["rev-parse", "--git-common-dir"])?;
        let p = PathBuf::from(out.trim());
        Ok(if p.is_absolute() {
            p
        } else {
            self.root.join(p)
        })
    }

    /// The sha `HEAD` currently points at.
    ///
    /// # Errors
    ///
    /// Fails if there is no commit yet, or git will not answer.
    pub fn head(&self) -> Result<Sha> {
        let out = run(&self.root, ["rev-parse", "HEAD"])?;
        Sha::parse(&out).map_err(|got| VcsError::Unreadable {
            args: "rev-parse HEAD".to_owned(),
            got,
        })
    }

    /// 🚨 **Take a checkpoint: the temp-index snapshot, written to a ref.**
    ///
    /// ```text
    /// GIT_INDEX_FILE=<scratch>  git read-tree HEAD
    /// GIT_INDEX_FILE=<scratch>  git add -A
    /// GIT_INDEX_FILE=<scratch>  git write-tree                           -> tree
    /// GIT_INDEX_FILE=<scratch>  git commit-tree <tree> -p HEAD -m "..."  -> the sha
    ///                           git update-ref <ref_name> <sha>
    /// ```
    ///
    /// Measured **0.16 s** on a 495-file tree and 0.31 s on a 1,079-file one. It
    /// touches neither the index nor the working tree, and **it captures the
    /// files the agent created — which `git stash create` cannot** (F329).
    ///
    /// `ref_name` should be under [`CHECKPOINT_REFS`]; see [`checkpoint_ref`].
    /// The `update-ref` is the line that stops `git gc --prune=now` collecting
    /// the snapshot, so it is done here rather than left to a caller.
    ///
    /// # Errors
    ///
    /// Fails if any of the five commands fails, or if the scratch index cannot be
    /// placed.
    pub fn checkpoint(&self, ref_name: &str, message: &str) -> Result<Sha> {
        let scratch = self.scratch_path("snapshot", "index")?;
        // The scratch index must not be inside the working tree: `git add -A`
        // would otherwise be asked to stage the file it is writing.
        let index = ScratchFile(scratch);

        let head = self.head()?;
        run_with_index(&self.root, index.path(), ["read-tree", "HEAD"])?;
        run_with_index(&self.root, index.path(), ["add", "-A"])?;
        let tree = run_with_index(&self.root, index.path(), ["write-tree"])?;
        let tree = tree.trim();

        let sha = run_with_index(
            &self.root,
            index.path(),
            ["commit-tree", tree, "-p", head.as_str(), "-m", message],
        )?;
        let sha = Sha::parse(&sha).map_err(|got| VcsError::Unreadable {
            args: "commit-tree".to_owned(),
            got,
        })?;

        run(&self.root, ["update-ref", ref_name, sha.as_str()])?;
        Ok(sha)
    }

    /// What changed between two snapshots.
    ///
    /// 🚨 **Never diff a snapshot against `HEAD`** — the v1 donor's tree would
    /// report 241 phantom files (F329) — **and never ask `git status` what
    /// changed** (F328). Both trees named here are snapshots.
    ///
    /// # Errors
    ///
    /// Fails if git will not answer or prints a status line this cannot read.
    pub fn changed_between(&self, from: &Sha, to: &Sha) -> Result<Vec<Change>> {
        // -z so a path containing a newline or a quote is not a parsing problem,
        // and --no-renames off so a rename reads as one change rather than two.
        let out = run(
            &self.root,
            [
                "diff",
                "--name-status",
                "-z",
                "--find-renames",
                from.as_str(),
                to.as_str(),
            ],
        )?;

        let mut fields = out.split('\0').filter(|s| !s.is_empty());
        let mut changes = Vec::new();
        while let Some(code) = fields.next() {
            // R and C statuses are followed by *two* paths: the source and the
            // destination. Everything else is followed by one.
            let (path, from_path) = if code.starts_with('R') || code.starts_with('C') {
                let src = fields.next().unwrap_or_default().to_owned();
                let dst = fields.next().unwrap_or_default().to_owned();
                (dst, Some(src))
            } else {
                (fields.next().unwrap_or_default().to_owned(), None)
            };
            if path.is_empty() {
                return Err(VcsError::Unreadable {
                    args: "diff --name-status -z".to_owned(),
                    got: format!("status {code} with no path"),
                });
            }
            changes.push(Change {
                kind: ChangeKind::from_status(code, from_path.as_deref()),
                path,
            });
        }
        Ok(changes)
    }

    /// The same change, as a patch: **the pre-image and the post-image in one
    /// artifact.**
    ///
    /// 🚨 This is what the Judge reads, and it is a *diff* rather than a score
    /// for a measured reason: pointwise scoring is 0 of 8, and pairwise against
    /// the pre-image is 14 of 14 on the same defects and order-stable on 7 of 7
    /// pairs (ADR-0008). A unified diff is the cheapest honest form of *show it
    /// the other artifact* — the `-` lines are the pre-image and the `+` lines
    /// are the post-image, in one pass over the same object database
    /// [`changed_between`](Self::changed_between) already walked.
    ///
    /// ⚠ **Every flag here pins something an operator's git configuration can
    /// change**, because the artifact a reviewer reads must not depend on whose
    /// machine produced it: `diff.external` would answer a different question
    /// entirely, `color.diff = always` would put escape sequences in the middle
    /// of a prompt, and `diff.context` would silently move how much of the
    /// pre-image is shown. `--find-renames` matches `changed_between` so the two
    /// describe one change.
    ///
    /// # Errors
    ///
    /// Fails if git will not answer.
    pub fn patch_between(&self, from: &Sha, to: &Sha) -> Result<String> {
        run(
            &self.root,
            [
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--find-renames",
                "--unified=3",
                from.as_str(),
                to.as_str(),
            ],
        )
    }

    /// Put the working tree back to a snapshot. Measured at **0.06 s**.
    ///
    /// ⚠ This is exact *up to the repository's own text attributes* — see the
    /// module docs. It is destructive by design: it is the operator's undo, and
    /// the console shows exactly which paths will change before it is pressed.
    ///
    /// # Errors
    ///
    /// Fails if git will not check the tree out.
    pub fn restore(&self, to: &Sha) -> Result<()> {
        run(&self.root, ["read-tree", "-u", "--reset", to.as_str()])?;
        Ok(())
    }

    /// Whether the working tree and index hold nothing of their own.
    ///
    /// 🚨 **This is the one place the crate asks `git status` anything, and it is
    /// the one question `status` can answer soundly.** F328 is that *clean* is
    /// not a claim about content — 241 of 539 tracked files in the v1 donor
    /// differed from their blobs while it said clean, because it answers from a
    /// stat cache. That makes it useless for *has this file changed* and exactly
    /// right for *is there anything here I would destroy*: the question is about
    /// the operator's uncommitted work, and a stale cache errs toward refusing.
    ///
    /// `--untracked-files=normal` is passed explicitly so that somebody's
    /// `status.showUntrackedFiles` cannot quietly widen what counts as clean.
    ///
    /// # Errors
    ///
    /// Fails if git will not answer.
    pub fn is_clean(&self) -> Result<bool> {
        let out = run(
            &self.root,
            ["status", "--porcelain", "--untracked-files=normal"],
        )?;
        Ok(out.trim().is_empty())
    }

    /// Apply a patch to the working tree **and the index**, three-way where the
    /// context has moved.
    ///
    /// 🚨 **`--3way` is what lets an old attempt land at all.** A checkpoint pair
    /// is parented on the HEAD its attempt forked from, which may be a long way
    /// behind the branch by the time anybody lands it — the oldest green pair on
    /// this project's own log is 58 commits back. A plain apply fails on the
    /// first moved line; a three-way apply reconstructs the pre-image from the
    /// blobs, and those blobs are in the object store precisely because
    /// [`Repo::checkpoint`] wrote a ref (F330).
    ///
    /// 🚨 **A conflict is NOT all-or-nothing here, and the caller must undo it.**
    /// The plain applier stages nothing unless the whole patch applies; the
    /// three-way fallback is a *merge*, so a hunk it cannot reconcile is written
    /// into the file with conflict markers and left as an unmerged index entry —
    /// `Applied patch to 'x.rs' with conflicts. U x.rs`, exit 1. This function
    /// reports that as an error and **deliberately does not clean up**: the undo
    /// is a reset to `HEAD`, which is only safe for a caller that established the
    /// tree was clean first, and this crate cannot know that. See
    /// `abcc::land`, which asks [`Repo::is_clean`] and then resets.
    ///
    /// 🚨 **The patch goes through a file, and that is load-bearing rather than
    /// convenient.** `git apply --3way` tries a direct apply first and *re-reads
    /// the patch* to fall back, so a patch arriving on **stdin cannot be fallen
    /// back to** — the fallback silently does not happen and git reports the
    /// direct apply's conflict. Measured on this repository's own five green
    /// checkpoint pairs, whose parents are 16 to 58 commits behind `main`: piped
    /// on stdin, **5 of 5 report `patch failed`**; written to a file, **5 of 5
    /// apply clean**. Same patches, same tree, same flags. ⚠ So a future
    /// refactor that "simplifies" this into a pipe turns every landing of an
    /// older attempt into a conflict that is not there — and the tests would
    /// still pass, because a patch taken minutes ago applies directly.
    ///
    /// # Errors
    ///
    /// Fails if the scratch file cannot be placed, or if git will not apply the
    /// patch — every conflict included.
    pub fn apply(&self, patch: &str) -> Result<()> {
        let scratch = ScratchFile(self.scratch_path("landing", "patch")?);
        std::fs::write(scratch.path(), patch).map_err(|source| VcsError::Scratch {
            path: scratch.path().to_path_buf(),
            source,
        })?;
        run(
            &self.root,
            [
                OsStr::new("apply"),
                OsStr::new("--index"),
                OsStr::new("--3way"),
                scratch.path().as_os_str(),
            ],
        )?;
        Ok(())
    }

    /// Commit whatever is staged, and answer with the sha it made.
    ///
    /// ⚠ **This is the only function in the crate that moves a branch.**
    /// Everything else writes under [`CHECKPOINT_REFS`] or into a detached
    /// worktree, exactly so that a running attempt can never touch the
    /// operator's history. This is reached from an operator verb and from
    /// nothing else.
    ///
    /// `--no-verify` is deliberately **not** passed: if the repository has a
    /// pre-commit hook, a landing is the moment it should run.
    ///
    /// # Errors
    ///
    /// Fails if git will not commit — including the two ordinary reasons, an
    /// unconfigured `user.email` and an empty index.
    pub fn commit(&self, message: &str) -> Result<Sha> {
        run(&self.root, ["commit", "-m", message])?;
        self.head()
    }

    /// Create an isolated worktree at `sha`. Measured at **0.25 s**.
    ///
    /// `path` must be outside this repository's working tree. A detached
    /// worktree is what the attempt gets; nothing about the operator's checkout
    /// moves.
    ///
    /// # Errors
    ///
    /// Fails if git will not create the worktree.
    pub fn open_worktree(&self, path: &Path, sha: &Sha) -> Result<Worktree> {
        run(
            &self.root,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--detach"),
                path.as_os_str(),
                OsStr::new(sha.as_str()),
            ],
        )?;
        Ok(Worktree {
            repo_root: self.root.clone(),
            path: path.to_path_buf(),
            sha: sha.clone(),
        })
    }

    /// Take hold of a worktree this process did not create.
    ///
    /// 🚨 **The durable record of a worktree is the log, not a [`Worktree`]
    /// value.** [`open_worktree`](Self::open_worktree) hands one back to the
    /// process that cut it, and that process is the driver — but the operator's
    /// `abcc release` is a *different* process reading `WorktreeOpened { path,
    /// sha }` off the event log, and it has to be able to close what it finds
    /// there. Without this the only way to take a worktree down is to have been
    /// the one who put it up.
    ///
    /// It runs no git and checks nothing, deliberately: the check that matters
    /// is *will git remove this*, and that is [`Worktree::close`]'s, which is
    /// git's own answer at the moment of use. A second check here — reading
    /// `git worktree list` and comparing paths — would be a reimplementation
    /// that can disagree with the one that acts, on a platform where two
    /// spellings of one directory are routine.
    #[must_use]
    pub fn adopt_worktree(&self, path: &Path, sha: &Sha) -> Worktree {
        Worktree {
            repo_root: self.root.clone(),
            path: path.to_path_buf(),
            sha: sha.clone(),
        }
    }

    /// A scratch path outside the working tree, so `git add -A` is never asked to
    /// stage the file it is writing — and, for a patch, so the tree a patch
    /// describes never contains the patch.
    ///
    /// The common dir rather than the worktree's own `.git`: a linked worktree's
    /// `.git` is a file, and both callers want somewhere that exists for every
    /// shape of checkout.
    fn scratch_path(&self, what: &str, ext: &str) -> Result<PathBuf> {
        let dir = self.common_dir()?;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        Ok(dir.join(format!("abcc-{what}-{unique}.{ext}")))
    }
}

/// The ref a checkpoint is written to. One namespace, so `git for-each-ref` can
/// show every save game and a cleanup can find them all.
#[must_use]
pub fn checkpoint_ref(mission: &str, seq: i64) -> String {
    format!("{CHECKPOINT_REFS}/{mission}/{seq}")
}

/// An isolated working directory at a known sha.
#[derive(Debug)]
pub struct Worktree {
    repo_root: PathBuf,
    path: PathBuf,
    sha: Sha,
}

impl Worktree {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn sha(&self) -> &Sha {
        &self.sha
    }

    /// Remove the worktree **and prune its metadata**.
    ///
    /// BCF calls neither, so its `.git/worktrees/` accumulates; the strings
    /// `worktree remove` and `worktree prune` appear nowhere in that repository
    /// (F325). Both are here, and `prune` runs even when `remove` fails, because
    /// the failure mode this cleans up after is precisely a `remove` that did
    /// not happen.
    ///
    /// # Errors
    ///
    /// Fails if git will not remove the worktree. The prune is attempted either
    /// way and its own failure is not reported over the first one.
    pub fn close(self) -> Result<()> {
        let removed = run(
            &self.repo_root,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                self.path.as_os_str(),
            ],
        );
        let pruned = run(&self.repo_root, ["worktree", "prune"]);
        removed?;
        pruned?;
        Ok(())
    }
}

/// A temp index that deletes itself. The snapshot is only cheap because it never
/// touches the real index, and leaving these behind in `.git/` would be the same
/// accumulating-metadata defect BCF has with worktrees.
struct ScratchFile(PathBuf);

impl ScratchFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        // Best effort: a leftover scratch file is inert, and failing a
        // checkpoint or a landing because a temp file would not delete would be
        // worse than the file.
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Running git
// ---------------------------------------------------------------------------

/// Run git in `cwd` and return stdout.
///
/// `Command::output` reads both pipes to completion, which is the fix for F202's
/// donor defect from the other side: a child that writes past the 64 KiB pipe
/// buffer, blocks on `write`, and is recorded as a timeout for work it had
/// finished. `git diff --name-status` on a large tree is exactly that shape.
fn run<I, S>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_inner(cwd, args, None)
}

fn run_with_index<I, S>(cwd: &Path, index: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_inner(cwd, args, Some(index))
}

fn run_inner<I, S>(cwd: &Path, args: I, index: Option<&Path>) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<std::ffi::OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let shown = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    let mut cmd = Command::new("git");
    cmd.current_dir(cwd).args(&args);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }

    let out = cmd.output().map_err(|source| VcsError::Spawn {
        args: shown.clone(),
        source,
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(VcsError::Git {
            args: shown,
            code: out
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |c| c.to_string()),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}
