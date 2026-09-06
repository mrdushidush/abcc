//! The eighth verb: the operator takes the keyboard, **and gets the work with
//! it**.
//!
//! ADR-0012 §4 lists eight verbs against three mechanisms and puts *take over
//! manually* under the second — a typed lifecycle with checkpoints — beside
//! retry and edit-a-prompt. The lifecycle half has been here since Skeleton:
//! [`Command::Commandeer`] is legal from every non-terminal state,
//! [`TaskState::Commandeered`] is watched and never reaped, and
//! [`crate::ops::finish`] already sends the command so that `abcc accept` can
//! finish work by hand. What was missing is the half an operator can use: a
//! directory to stand in.
//!
//! # 🚨 Why the verb refuses where the lifecycle would not
//!
//! `Commandeer` is legal from `Engaged`. **This verb is not**, and the
//! difference is the whole reason it is a separate thing from the transition:
//! *the transition moves a state and the verb has to move a directory.* While a
//! task holds a slot, the fleet is holding its workspace — a live driver owns
//! that worktree and will remove it when the attempt lands. Cutting the operator
//! a second tree beside it would hand them a stale copy of work still being
//! written, and moving the task to `Commandeered` underneath a running driver
//! makes its landing command illegal.
//!
//! So the test is [`abcc_core::task::StateContract::holds_slot`] rather than a hand-written list
//! of states, and the refusal names the desk verb that frees it. The states this
//! leaves are exactly the ones where take-over means something: `Queued` (I will
//! do this one by hand), `AwaitingOrders` (the model asked me and I will finish
//! it), `Holding` (parked, and mine now), and `Commandeered` (already mine).
//!
//! ⚠ **This is deliberately not a desk verb.** `pause`, `halt`, `kill`,
//! `redirect` and `resume` are typed at a run or a sortie and addressed to an
//! attempt that is flying; take-over is only legal when nothing is flying. A
//! sixth word on that prompt would be a word that is refused every time it is
//! reachable.
//!
//! # 🚨 The order is the log first and the directory second
//!
//! [`crate::run::attempt`] is the only thing that calls `Store::boot`, so
//! **nothing sweeps up after an operator command.** That inverts the driver's
//! opening order (checkpoint → worktree → `AttemptStarted` → `Engage`, where
//! boot's orphan sweep is the backstop): a `Commandeered` task with no directory
//! is one `abcc release` away from fixed, and a directory cut for a task the log
//! never moved is an orphan with nobody's name on it.
//!
//! # What the operator is given, and where it comes from
//!
//! The tree is cut at the task's **last checkpoint** — the work as the fleet
//! left it. A task that has never run has no checkpoint, so one is taken from
//! the operator's checkout, which is exactly what [`abcc_drive::Driver`] gives
//! an attempt at `open_workspace`: the same recipe, through the same
//! [`abcc_drive::snapshot`], including uncommitted work.
//!
//! And if a worktree is already standing for this task, take-over **adopts it**
//! rather than cutting a second. That is not only idempotence: the driver
//! records a worktree it could not remove as a `Note` and leaves the directory
//! on disk, so the tree that is standing is sometimes fresher than the last
//! checkpoint.

use std::io::Write;
use std::path::PathBuf;

use abcc_core::event::Event;
use abcc_core::seq::TaskId;
use abcc_core::task::Command;
use abcc_store::{Store, TaskRow};
use abcc_tui::Theme;
use abcc_vcs::{Repo, Sha};

use crate::ops::{self, Ground};
use crate::{AppError, Invocation, cli};

/// A worktree the log says is open, with nobody in this process holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Standing {
    pub path: PathBuf,
    pub sha: String,
}

/// The operator takes the keyboard on `task`, and a tree to use it in.
///
/// # Errors
///
/// [`AppError::Refused`] if there is no such task, if it is finished, or if the
/// fleet is holding its slot; [`AppError::Vcs`] if git will not cut the tree.
pub fn take(
    invocation: &Invocation,
    task: cli::TaskRef,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let mut store = ops::open_log(&ground.home)?;
    let id = TaskId::at(abcc_core::seq::Seq::new(task.0));
    let row = ops::row(&store, id)?;

    refuse_unless_takeable(&row)?;

    // The log first. See the module docs: nothing boots after an operator
    // command, so the recoverable failure is the one that leaves the task moved
    // and the directory missing.
    let state = ops::command(&mut store, id, Command::Commandeer)?;

    let (path, sha, how) = match standing(&store, id)? {
        // A tree is already up for this task — the operator's own from an
        // earlier `take`, or one the driver could not remove. Either way it is
        // the tree that holds the work, and cutting a second beside it would
        // put the operator in the older of two directories.
        Some(open) if open.path.exists() => (open.path, open.sha, How::Adopted),
        Some(open) => {
            // The log says open and the directory is not there. Say so and close
            // it on the log, so the record and the disk agree again before a new
            // one is cut.
            store.append(Event::Note {
                text: format!(
                    "{id}: the worktree at {} is on the log and not on disk; cutting a new one",
                    open.path.display()
                ),
            })?;
            store.append(Event::WorktreeClosed {
                task: id,
                path: open.path.display().to_string(),
            })?;
            let (path, sha) = cut(&mut store, &ground, &row)?;
            (path, sha, How::Cut)
        }
        None => {
            let (path, sha) = cut(&mut store, &ground, &row)?;
            (path, sha, How::Cut)
        }
    };

    writeln!(out, "{id}  {}", Theme::Command.state(&state))?;
    writeln!(out, "workspace  {}", path.display())?;
    writeln!(out, "at         {sha}  ({})", how.provenance())?;
    writeln!(
        out,
        "\nThe fleet will not admit it while you hold it. `abcc release {id}` hands it back\n\
         queued, snapshotting whatever you did first; `abcc accept {id}` and `abcc reject {id}`\n\
         finish it."
    )?;
    Ok(())
}

/// The operator hands `task` back to the fleet.
///
/// # Errors
///
/// [`AppError::Refused`] if there is no such task or the operator does not hold
/// it, [`AppError::Vcs`] if git will not take the closing snapshot.
pub fn release(
    invocation: &Invocation,
    task: cli::TaskRef,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let mut store = ops::open_log(&ground.home)?;
    let id = TaskId::at(abcc_core::seq::Seq::new(task.0));
    let row = ops::row(&store, id)?;

    // 🚨 **The legality is settled before anything is touched, and it is settled
    // by the transition function rather than by a `match` here.** `hand_back`
    // snapshots a tree and removes it; running it against a task the operator
    // does not hold would take the *driver's* worktree down under a live attempt
    // and only then discover that `Release` is not legal from `Engaged`. A dry
    // run costs nothing and keeps `TaskState::apply` the one authority on where a
    // command is allowed.
    if let Err(refusal) = row.state.apply(&Command::Release, store.head()?) {
        return Err(AppError::Refused(format!(
            "{refusal}. `abcc take {id}` is how you take a task over."
        )));
    }

    // 🚨 The workspace goes back before the task does. `Release` lands the task
    // in `Queued`, which is the one state a sortie admits — so a release that
    // moved the task first could have an attempt cutting its own worktree from a
    // checkpoint the operator had not written yet.
    let kept = hand_back(&mut store, &ground, &row, out)?;
    let state = ops::command(&mut store, id, Command::Release)?;

    writeln!(out, "{id}  {}", Theme::Command.state(&state))?;
    match kept {
        Some(sha) => writeln!(
            out,
            "Your work is at {sha}. The fleet may admit it again; the next attempt starts from\n\
             that snapshot."
        )?,
        None => writeln!(
            out,
            "You held no workspace, so there was nothing to snapshot. The fleet may admit it\n\
             again."
        )?,
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// the pieces the endings share
// ---------------------------------------------------------------------------

/// Snapshot whatever the operator has in their tree, then take the tree down.
///
/// Returns the sha of the closing snapshot, or `None` when there was no tree to
/// close — which is every task on a log written before this verb existed, and
/// every task the driver landed normally, because [`abcc_drive::Driver`] closes
/// its own worktree before it sends the landing command.
///
/// 🚨 **`abcc accept` and `abcc reject` call this too, and the reason is written
/// in the type**: all three terminal states have `holds_workspace: false` on
/// their [`StateContract`](abcc_core::task::StateContract), and until now that
/// was a claim nothing enforced. A task cannot go terminal still holding a
/// directory.
///
/// ⚠ A close git refuses is a `Note` and not a failure, exactly as it is in the
/// driver: the work is already in the snapshot above, and `Worktree::close`
/// prunes even when the remove fails.
///
/// # Errors
///
/// [`AppError::Store`] if the log will not take an event, [`AppError::Vcs`] if
/// git will not take the snapshot.
pub(crate) fn hand_back(
    store: &mut Store,
    ground: &Ground,
    row: &TaskRow,
    out: &mut impl Write,
) -> Result<Option<String>, AppError> {
    let Some(open) = standing(store, row.id)? else {
        return Ok(None);
    };
    let id = row.id;

    if !open.path.exists() {
        store.append(Event::Note {
            text: format!(
                "{id}: the worktree at {} was already gone, so nothing was snapshotted from it",
                open.path.display()
            ),
        })?;
        store.append(Event::WorktreeClosed {
            task: id,
            path: open.path.display().to_string(),
        })?;
        return Ok(None);
    }

    // `Repo::open` asks `rev-parse --show-toplevel`, so it treats "this is a
    // worktree" as the normal case, and the scratch index lands in the shared
    // git directory outside this tree.
    let inner = Repo::open(&open.path)?;
    let kept = abcc_drive::snapshot(store, &inner, row, "operator")?;

    let path = open.path.display().to_string();
    match ground.repo.adopt_worktree(&open.path, &kept.sha).close() {
        Ok(()) => {
            store.append(Event::WorktreeClosed { task: id, path })?;
        }
        Err(e) => {
            let text = format!("{id}: the worktree at {path} would not close: {e}");
            writeln!(out, "\u{26a0} {text}")?;
            store.append(Event::Note { text })?;
        }
    }
    Ok(Some(kept.sha.to_string()))
}

/// The worktree this task has open, folded out of its history.
///
/// ⚠ **A `WorktreeClosed` for the task clears whatever was open, whatever path
/// it names.** A task has at most one worktree at a time — the driver opens one
/// per attempt and closes it before the landing command, and [`take`] refuses
/// while the fleet holds the slot — so matching on the path would only add a way
/// for one spelling of a directory to leave a phantom standing forever.
///
/// # Errors
///
/// [`AppError::Store`] if the history will not read.
pub(crate) fn standing(store: &Store, task: TaskId) -> Result<Option<Standing>, AppError> {
    let mut open = None;
    for logged in store.task_history(task)? {
        match logged.event {
            Event::WorktreeOpened { path, sha, .. } => {
                open = Some(Standing {
                    path: PathBuf::from(path),
                    sha,
                });
            }
            Event::WorktreeClosed { .. } => open = None,
            _ => {}
        }
    }
    Ok(open)
}

// ---------------------------------------------------------------------------
// the pieces `take` has to itself
// ---------------------------------------------------------------------------

/// Where the tree the operator is standing in came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    Cut,
    Adopted,
}

impl How {
    fn provenance(self) -> &'static str {
        match self {
            How::Cut => "cut for you",
            How::Adopted => "the tree that was already standing",
        }
    }
}

/// The verb's legality, which is narrower than the transition's.
///
/// See the module docs: `Commandeer` is legal from `Engaged` and this is not,
/// because a directory cannot be handed over while a driver is writing in it.
/// The test is the contract's rather than a list of state names, so a tenth
/// state is admitted or refused by what it says about itself.
fn refuse_unless_takeable(row: &TaskRow) -> Result<(), AppError> {
    let contract = row.state.contract();
    if contract.terminal {
        return Err(AppError::Refused(format!(
            "{} is {} and finished. There is nothing to take over — `abcc replay {}` says how it \
             got there.",
            row.id,
            row.state.name(),
            row.id
        )));
    }
    if contract.holds_slot {
        return Err(AppError::Refused(format!(
            "{} is {} and the fleet is holding its workspace. Stop the attempt first — `halt {}` \
             at the run or fleet desk keeps the work at a checkpoint — and take it over once it \
             has landed.",
            row.id,
            row.state.name(),
            row.id
        )));
    }
    Ok(())
}

/// Cut the operator a tree, at the last checkpoint or at a fresh one.
fn cut(store: &mut Store, ground: &Ground, row: &TaskRow) -> Result<(PathBuf, String), AppError> {
    let sha = match last_checkpoint(store, row.id)? {
        Some(sha) => sha,
        // Nothing has ever run this task, so there is no snapshot of it. Take
        // one from the operator's checkout, which is what `Driver::open_workspace`
        // gives an attempt — uncommitted work included.
        None => abcc_drive::snapshot(store, &ground.repo, row, "before")?.sha,
    };

    let dir = ground.home.worktrees();
    std::fs::create_dir_all(&dir)?;
    // The suffix is for a human reading a directory listing, and nothing reads
    // it back: an attempt's tree is `{task}-{seq}` and this is `{task}-take-{seq}`,
    // so the two cannot collide and an operator can tell whose is whose. What
    // says a worktree is the operator's is the log and the task's state, never
    // this name.
    let path = dir.join(format!("{}-take-{}", row.id, store.head()?.get()));
    let worktree = ground.repo.open_worktree(&path, &sha)?;
    store.append(Event::WorktreeOpened {
        task: row.id,
        path: path.display().to_string(),
        sha: sha.to_string(),
    })?;
    Ok((worktree.path().to_path_buf(), sha.to_string()))
}

/// The sha of the last checkpoint taken for this task.
fn last_checkpoint(store: &Store, task: TaskId) -> Result<Option<Sha>, AppError> {
    let mut last = None;
    for logged in store.task_history(task)? {
        if let Event::CheckpointTaken { sha, .. } = logged.event {
            last = Some(sha);
        }
    }
    last.map(|raw| {
        Sha::parse(&raw).map_err(|why| {
            AppError::Refused(format!(
                "{task}'s last checkpoint is recorded as {raw:?}, which is not a sha: {why}"
            ))
        })
    })
    .transpose()
}
