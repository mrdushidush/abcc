//! The attempt driver: **Localize → Change**, over the log, the repository and
//! the engine.
//!
//! Four crates each hold one thing and none of them holds a run.
//! [`abcc_core`](../abcc_core/index.html) has the words, `abcc-store` writes them
//! down, `abcc-vcs` gives an attempt somewhere to work, and `abcc-engine` drives
//! one phase against a model. This crate is where those four meet, and it is the
//! last piece of the Skeleton milestone's engine: *one slot running one attempt
//! through Localize → Change on a real repo* (`PLAN.md` §3).
//!
//! It is a separate crate rather than a module of the engine on purpose. The turn
//! loop writes through [`abcc_engine::turn::Journal`] precisely so that it does
//! not depend on a `Store`; putting the composition inside `abcc-engine` would
//! put `rusqlite` behind that trait and make the seam decorative.
//!
//! # The five rules this driver holds
//!
//! 1. 🚨 **It never says `Accomplished`.** There is no gate at Skeleton, so an
//!    attempt whose model answered ends
//!    [`AttemptOutcome::Uncertain`]`{ why: `[`Why::NoCheckerForArtifact`]` }` and
//!    the task is handed to the operator. Calling that success would be exactly
//!    the defect ADR-0009 exists to prevent — a model's claim reaching the place
//!    a measurement belongs — and it is the one shortcut that would make the
//!    whole milestone dishonest.
//! 2. **A failure before `AttemptStarted` leaves the task `Deployed`, and that is
//!    the design rather than a leak.** `Deployed`'s contract *is* "a slot is held
//!    and no attempt has started"; it is reaped on the spin-up bound and
//!    requeued by boot. Inventing a third recovery path here is how v1 ended up
//!    with a second one that had never run when it was needed (F166).
//! 3. **The checkpoint is the durable form of the work; the worktree is
//!    disposable.** Every ending except `Kill` takes a closing snapshot *in the
//!    worktree* before the worktree is removed, and writes it to a ref so
//!    `git gc --prune=now` cannot collect it (ADR-0007, F330).
//! 4. **The operator's verb is acknowledged on the log before anything acts on
//!    it.** `ControlApplied` is written first, then the work is disposed of the
//!    way [`Keep`] says.
//! 5. **What happens next is returned, not done.** [`Landed::next`] is a
//!    [`NextAction`] the fleet has not been built to receive yet — admission,
//!    slots and retry budget 2 are the Fleet milestone — so the driver runs one
//!    attempt and hands back its recommendation rather than acting on it.

use std::fs;
use std::path::PathBuf;

use abcc_core::attempt::{AttemptOutcome, Cause, NextAction};
use abcc_core::event::Event;
use abcc_core::outcome::Why;
use abcc_core::run::AttemptPhase;
use abcc_core::seq::{AttemptId, CheckpointId, PromptId, TaskId, UnitId};
use abcc_core::task::{AbortReason, Command, Refused, TaskState};
use abcc_engine::control::{ControlPoint, Keep, Watch};
use abcc_engine::head::Head;
use abcc_engine::provider::{Body, Provider};
use abcc_engine::turn::{Limits, PhaseEnded, PhaseReport, Tools, TurnLoop};
use abcc_engine::workspace::Workspace;
use abcc_store::{Applied, Store, StoreError, TaskRow};
use abcc_vcs::{Repo, Sha, VcsError, Worktree, checkpoint_ref};

pub mod brief;
pub mod journal;

pub use journal::StoreJournal;

/// Anything that stops the driver from running an attempt at all.
///
/// A *refused command* is in here and a *refused tool call* is not: the second is
/// a normal thing for a model to try and is recorded as [`Why::Denied`] on the
/// log, while the first means the caller asked the driver to run a task that was
/// not theirs to run.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    #[error("the log: {0}")]
    Store(#[from] StoreError),
    #[error("git: {0}")]
    Vcs(#[from] VcsError),
    #[error("{what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("no task {0} in the projection")]
    NoSuchTask(TaskId),
    #[error("{command} was refused: {refusal}")]
    Refused {
        command: &'static str,
        refusal: Refused,
    },
}

type Result<T> = std::result::Result<T, DriveError>;

/// Where one attempt got to, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    pub attempt: AttemptId,
    pub outcome: AttemptOutcome,
    /// The task's state after the attempt ended — read back from the projection
    /// rather than assumed, because the transition function is the authority on
    /// where a command lands (ADR-0004).
    pub state: TaskState,
    pub localize: PhaseReport,
    /// `None` when Localize produced no artifact to hand over, which is the only
    /// way the Change phase does not run.
    pub change: Option<PhaseReport>,
    /// The closing snapshot's sha. `None` only when the operator said `Kill`,
    /// which is the one verb that keeps nothing.
    pub kept: Option<String>,
    /// 🚨 A **recommendation**, not an act. `None` when the ending was the
    /// operator's, because what happens after an operator stops something is the
    /// operator's and not the fleet's to propose.
    pub next: Option<NextAction>,
}

/// One attempt, from `Queued` to wherever it honestly ends.
pub struct Driver<'a> {
    store: &'a mut Store,
    repo: &'a Repo,
    provider: &'a dyn Provider,
    model: String,
    worktrees: PathBuf,
    limits: Limits,
}

impl<'a> Driver<'a> {
    /// `worktrees` is the directory attempt worktrees are made under, and it must
    /// be outside `repo`'s working tree — git refuses to nest one otherwise, and
    /// a worktree inside the tree it snapshots would be captured by the next
    /// `git add -A`.
    #[must_use]
    pub fn new(
        store: &'a mut Store,
        repo: &'a Repo,
        provider: &'a dyn Provider,
        model: impl Into<String>,
        worktrees: impl Into<PathBuf>,
    ) -> Driver<'a> {
        Driver {
            store,
            repo,
            provider,
            model: model.into(),
            worktrees: worktrees.into(),
            limits: Limits::default(),
        }
    }

    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Driver<'a> {
        self.limits = limits;
        self
    }

    /// Run one attempt on `task` in `unit`, and land the task where its ending
    /// implies.
    ///
    /// The sequence is checkpoint → worktree → `AttemptStarted` → `Engage`, in
    /// that order, because everything before `AttemptStarted` can fail with
    /// nothing to tombstone: rule 2 in the module docs.
    ///
    /// # Errors
    ///
    /// [`DriveError`] — the log would not take a write, git would not give us a
    /// worktree, or a lifecycle command was refused.
    pub fn run(
        &mut self,
        task: TaskId,
        unit: UnitId,
        cause: Cause,
        control: &mut ControlPoint,
    ) -> Result<Landed> {
        let row = self.row(task)?;
        self.command(task, Command::Deploy { unit })?;

        let opened = self.open_workspace(&row, control.watch())?;

        let started = self.store.append(Event::AttemptStarted {
            task,
            unit,
            cause,
            checkpoint_from: Some(opened.checkpoint),
        })?;
        let attempt = AttemptId::at(started.seq);
        self.command(task, Command::Engage { attempt })?;

        // Two phases, two heads, two bodies. The body cannot be shared across
        // them: a phase is a different frozen prefix, so continuing one body into
        // the other would be a cold prefill wearing a warm one's clothes.
        let mut recon = Body::opening(brief::localize(&row));
        let localize = self.phase(
            attempt,
            AttemptPhase::Localize,
            Head::Recon,
            &mut recon,
            &opened.workspace,
            control,
        )?;

        let change = match &localize {
            PhaseEnded::Answered { text, .. } => {
                let mut builders = Body::opening(brief::change(&row, text));
                Some(self.phase(
                    attempt,
                    AttemptPhase::Change,
                    Head::Builders,
                    &mut builders,
                    &opened.workspace,
                    control,
                )?)
            }
            // No artifact to hand over. Running Builders on an absence would
            // spend a second model call to produce a second absence.
            PhaseEnded::Stopped { .. } | PhaseEnded::Unmeasured { .. } => None,
        };

        self.land(&row, attempt, opened, &localize, change.as_ref())
    }

    // -- the pieces --------------------------------------------------------

    /// Take the opening snapshot, cut a worktree at it, and open the tool layer
    /// over that worktree.
    fn open_workspace(&mut self, row: &TaskRow, watch: Watch) -> Result<Opened> {
        let taken = self.checkpoint(row, "before")?;

        fs::create_dir_all(&self.worktrees).map_err(|source| DriveError::Io {
            what: "creating the worktree directory",
            source,
        })?;
        let path = self
            .worktrees
            .join(format!("{}-{}", row.id, taken.id.born()));
        let worktree = self.repo.open_worktree(&path, &taken.sha)?;
        self.store.append(Event::WorktreeOpened {
            task: row.id,
            path: path.display().to_string(),
            sha: taken.sha.to_string(),
        })?;

        let workspace = Workspace::open(worktree.path())
            .map_err(|source| DriveError::Io {
                what: "opening the attempt's workspace",
                source,
            })?
            // The operator's urgent verb reaches a running tool child through
            // this, which is the whole of `shared_child`'s justification and was
            // measured at 4 ms to the child (F491).
            .watching(watch);

        Ok(Opened {
            checkpoint: taken.id,
            worktree,
            workspace,
        })
    }

    /// One phase: enter it on the log, run the turn loop, and insist that every
    /// event the loop produced actually landed.
    fn phase(
        &mut self,
        attempt: AttemptId,
        phase: AttemptPhase,
        head: Head,
        body: &mut Body,
        tools: &dyn Tools,
        control: &mut ControlPoint,
    ) -> Result<PhaseEnded> {
        self.store
            .append(Event::AttemptPhaseEntered { attempt, phase })?;

        let provider = self.provider;
        let model = self.model.clone();
        let limits = self.limits;

        let mut journal = StoreJournal::new(self.store);
        // ⚠ No schema. Schema-constrained decoding works and the donor uses it
        // nowhere (W1 F86), but the thing a schema constrains is an artifact with
        // a declared shape — the task set, a measurement — and Skeleton's
        // artifact is prose an operator reads. It arrives with the Gate.
        let ended = TurnLoop::new(provider, tools, model).limits(limits).run(
            head,
            attempt,
            None,
            body,
            control,
            &mut journal,
        );
        journal.into_result()?;
        Ok(ended)
    }

    /// End the attempt and put the task where its ending implies.
    fn land(
        &mut self,
        row: &TaskRow,
        attempt: AttemptId,
        opened: Opened,
        localize: &PhaseEnded,
        change: Option<&PhaseEnded>,
    ) -> Result<Landed> {
        let task = row.id;
        let last = change.unwrap_or(localize);

        // Rule 4: the verb is acknowledged before anything acts on it.
        if let PhaseEnded::Stopped { stop, .. } = last {
            self.store.append(Event::ControlApplied {
                task,
                control: stop.control.clone(),
            })?;
        }

        let keep = keep_for(last);
        let kept = match &keep {
            // 🚨 The only verb that keeps nothing. Everything else snapshots
            // first: 0.16 s (F490) against a tree nobody can look at afterwards.
            Keep::Nothing => None,
            Keep::AtCheckpoint | Keep::AndFork { .. } => {
                Some(self.checkpoint_worktree(row, &opened.worktree)?)
            }
        };

        let path = opened.worktree.path().display().to_string();
        match opened.worktree.close() {
            Ok(()) => {
                self.store.append(Event::WorktreeClosed { task, path })?;
            }
            // A worktree that will not go is worth saying out loud and is not
            // worth failing an attempt over — the work is already in the
            // checkpoint. `close` prunes even when the remove fails, so the
            // metadata does not accumulate the way BCF's does (F325).
            Err(e) => {
                self.store.append(Event::Note {
                    text: format!("the worktree at {path} would not close: {e}"),
                })?;
            }
        }

        let ending = ending(last, attempt, kept.as_ref());
        self.store.append(Event::AttemptEnded {
            task,
            attempt,
            outcome: ending.outcome.clone(),
        })?;

        let state = match ending.landing {
            Landing::HandToOperator { question } => {
                let asked = self.store.append(Event::OperatorPrompted {
                    task,
                    attempt,
                    question,
                })?;
                self.command(
                    task,
                    Command::RequestOrders {
                        attempt,
                        prompt: PromptId::at(asked.seq),
                    },
                )?
            }
            Landing::Failed => self.command(task, Command::Fail { attempt })?,
            Landing::Hold { checkpoint } => self.command(task, Command::Hold { checkpoint })?,
            Landing::Abort => self.command(
                task,
                Command::Abort {
                    reason: AbortReason::Operator {
                        by: OPERATOR.to_owned(),
                    },
                },
            )?,
        };

        Ok(Landed {
            attempt,
            outcome: ending.outcome,
            state,
            localize: localize.report().clone(),
            change: change.map(|c| c.report().clone()),
            kept: kept.map(|k| k.sha.to_string()),
            next: ending.next,
        })
    }

    /// The closing snapshot, taken **in the worktree**.
    ///
    /// [`Repo::open`] asks `rev-parse --show-toplevel`, so it treats "this is a
    /// worktree" as the normal case — which under ADR-0007 it is — and the
    /// scratch index lands in the shared git directory, outside this working
    /// tree, which is what keeps `git add -A` from being asked to stage the file
    /// it is writing.
    fn checkpoint_worktree(&mut self, row: &TaskRow, worktree: &Worktree) -> Result<Kept> {
        let inner = Repo::open(worktree.path())?;
        self.snapshot(&inner, row, "after")
    }

    fn checkpoint(&mut self, row: &TaskRow, when: &str) -> Result<Kept> {
        let repo = self.repo.clone();
        self.snapshot(&repo, row, when)
    }

    /// Snapshot `repo`, write it to a ref, and record it.
    ///
    /// 🚨 The ref name is disambiguated by the log head at the moment it is
    /// taken. Every checkpoint appends this event, so two of them always have an
    /// event between them and two names cannot collide — and the name then says
    /// *where in the replay* the snapshot was taken, which under ADR-0005 is the
    /// same fact as when.
    fn snapshot(&mut self, repo: &Repo, row: &TaskRow, when: &str) -> Result<Kept> {
        let at = self.store.head()?;
        let git_ref = checkpoint_ref(&row.mission.to_string(), at.get());
        let sha = repo.checkpoint(&git_ref, &format!("abcc {} {when} {at}", row.id))?;
        let logged = self.store.append(Event::CheckpointTaken {
            task: row.id,
            sha: sha.to_string(),
            git_ref,
        })?;
        Ok(Kept {
            id: CheckpointId::at(logged.seq),
            sha,
        })
    }

    // -- the store, through one door ---------------------------------------

    fn row(&self, task: TaskId) -> Result<TaskRow> {
        self.store.task(task)?.ok_or(DriveError::NoSuchTask(task))
    }

    /// Send a lifecycle command and read back where it landed.
    ///
    /// The state comes from the projection rather than from the command, because
    /// [`TaskState::apply`] is the authority on where a command goes and a second
    /// copy of that table here is a second copy that can drift.
    fn command(&mut self, task: TaskId, command: Command) -> Result<TaskState> {
        let name = command.name();
        match self.store.apply(task, command)? {
            Applied::Moved(_) => Ok(self.row(task)?.state),
            Applied::Refused { refusal, .. } => Err(DriveError::Refused {
                command: name,
                refusal,
            }),
        }
    }
}

/// Who a stop is attributed to. One constant, because every verb on the control
/// channel came from the console and there is nobody else it could be.
const OPERATOR: &str = "operator";

/// The attempt's isolation, alive for as long as the attempt is.
struct Opened {
    checkpoint: CheckpointId,
    worktree: Worktree,
    workspace: Workspace,
}

/// A snapshot that was taken and recorded.
struct Kept {
    id: CheckpointId,
    sha: Sha,
}

/// Where the driver takes the task when the attempt is over. Four, and none of
/// them is `Accomplished`.
enum Landing {
    /// Nothing measured it, so a human is owed the question.
    HandToOperator { question: String },
    /// The attempt produced no artifact.
    Failed,
    /// The operator stopped it and the work is at a checkpoint.
    Hold { checkpoint: CheckpointId },
    /// The operator stopped it and kept nothing.
    Abort,
}

/// The three answers one ending produces: what the attempt was, where the task
/// goes, and what the fleet should do about it. They are computed together
/// because they are one judgement, and splitting them is how two of them start
/// disagreeing.
struct Ending {
    outcome: AttemptOutcome,
    next: Option<NextAction>,
    landing: Landing,
}

/// What to do with the work, given how the phase ended.
fn keep_for(last: &PhaseEnded) -> Keep {
    match last {
        PhaseEnded::Stopped { stop, .. } => stop.keep.clone(),
        // Not the operator's ending, so the work is kept either way. A failed
        // attempt whose tree nobody can look at is a failure report with the
        // evidence deleted.
        PhaseEnded::Answered { .. } | PhaseEnded::Unmeasured { .. } => Keep::AtCheckpoint,
    }
}

fn ending(last: &PhaseEnded, attempt: AttemptId, kept: Option<&Kept>) -> Ending {
    match last {
        // 🚨 The model answered, and nothing measured it. This is the honest
        // ending of a *working* Skeleton attempt, and it is `Uncertain`.
        PhaseEnded::Answered { .. } => {
            let artifact = kept.map_or_else(
                || "the attempt's worktree".to_owned(),
                |k| k.sha.as_str().to_owned(),
            );
            let question = brief::unverified(&artifact);
            Ending {
                outcome: AttemptOutcome::Uncertain {
                    why: Why::NoCheckerForArtifact { artifact },
                },
                next: Some(NextAction::HandToOperator {
                    question: question.clone(),
                }),
                landing: Landing::HandToOperator { question },
            }
        }
        PhaseEnded::Unmeasured { why, .. } => {
            let next = next_after(why, attempt);
            Ending {
                outcome: classify(why),
                // ⚠ The landing *follows* the next action rather than being
                // asserted beside it. `Landing::Failed` under a
                // `HandToOperator` would be the two disagreeing, which is the
                // thing this struct's doc comment exists to prevent.
                landing: match &next {
                    NextAction::HandToOperator { question } => Landing::HandToOperator {
                        question: question.clone(),
                    },
                    NextAction::Attempt { .. } | NextAction::Stop => Landing::Failed,
                },
                next: Some(next),
            }
        }
        PhaseEnded::Stopped { stop, .. } => Ending {
            outcome: AttemptOutcome::Uncertain {
                why: Why::Cancelled {
                    by: OPERATOR.to_owned(),
                },
            },
            // ⚠ `Cause::Edit` names no prompt, and the redirect's prompt is not
            // lost by that: it is on the log, in the `ControlApplied` this
            // landing already wrote.
            next: match &stop.keep {
                Keep::AndFork { .. } => Some(NextAction::Attempt {
                    cause: Cause::Edit { of: attempt },
                }),
                Keep::AtCheckpoint | Keep::Nothing => None,
            },
            landing: match (&stop.keep, kept) {
                (Keep::Nothing, _) => Landing::Abort,
                (_, Some(k)) => Landing::Hold { checkpoint: k.id },
                // Unreachable through `keep_for`, and stated rather than
                // asserted: a hold whose checkpoint was not taken is not a hold.
                (_, None) => Landing::Failed,
            },
        },
    }
}

/// Which of the four attempt outcomes a phase's `Why` is.
///
/// 🚨 Exhaustive on purpose. `Uncertain` is retryable and `HardFailure` is not,
/// which is the distinction v1's single boolean could not carry — and a
/// thirteenth `Why` should not be able to arrive here and be quietly called a
/// soft failure.
fn classify(why: &Why) -> AttemptOutcome {
    match why {
        // Absences. None of them is a statement about the work.
        Why::NoCheckerForArtifact { .. }
        | Why::CheckerNotOnHost { .. }
        | Why::SpawnFailed { .. }
        | Why::NothingToRun { .. }
        | Why::FailedBeforeRunning { .. }
        | Why::BudgetExhausted { .. }
        | Why::Cancelled { .. }
        | Why::TruncatedAtCap { .. }
        | Why::StaleMeasurement { .. }
        // 🚨 F497 and F496 are absences for the same reason as the nine above:
        // neither says anything about the work. A phase that said nothing was
        // not measured and did not fail, and a conversation that filled the
        // window was cut off rather than judged.
        | Why::SaidNothing { .. }
        | Why::ContextOverflow { .. } => AttemptOutcome::Uncertain { why: why.clone() },
        // The stream went quiet. Another attempt on a less loaded box is a
        // plausible fix, which is what makes it soft.
        Why::Timeout { .. } => AttemptOutcome::SoftFailure { why: why.clone() },
        // A role reaching for a tool its ceiling does not admit, and a fault in
        // this engine, both repeat on the next attempt unchanged.
        Why::Denied { .. } | Why::EngineError { .. } => {
            AttemptOutcome::HardFailure { why: why.clone() }
        }
    }
}

/// What the fleet should do about it, which is a *separate* judgement from what
/// the attempt was: `CheckerNotOnHost` is uncertain and retrying it installs
/// nothing.
fn next_after(why: &Why, attempt: AttemptId) -> NextAction {
    match why {
        Why::BudgetExhausted { .. }
        | Why::TruncatedAtCap { .. }
        | Why::FailedBeforeRunning { .. }
        | Why::StaleMeasurement { .. }
        // The budget went into the reasoning trace and the answer never
        // arrived. The trace's size varies 9,942–16,564 characters on
        // *identical* input (F246), so a second sample is a plausible fix in
        // exactly the way it is for `TruncatedAtCap` above.
        | Why::SaidNothing { .. }
        | Why::Timeout { .. } => NextAction::Attempt {
            cause: Cause::Retry { of: attempt },
        },
        // 🚨 The one `Why` whose fix is neither another attempt nor giving up:
        // the same body works against a larger window, and loading one is the
        // operator's to do. Retrying against the same server repeats it
        // unchanged; stopping throws away work that is one setting from
        // running.
        Why::ContextOverflow {
            window,
            prompt_tokens,
        } => NextAction::HandToOperator {
            question: brief::overflowed(*window, *prompt_tokens),
        },
        Why::NoCheckerForArtifact { .. }
        | Why::CheckerNotOnHost { .. }
        | Why::SpawnFailed { .. }
        | Why::NothingToRun { .. }
        | Why::Cancelled { .. }
        | Why::Denied { .. }
        | Why::EngineError { .. } => NextAction::Stop,
    }
}
