//! The nine-variant task lifecycle, its per-state contract, and the single
//! transition function every write path goes through.
//!
//! ADR-0004. The names are David's, ratified 2026-08-28 (OQ-W3-6 closed), and
//! they are load-bearing rather than decorative: they were chosen against the 96
//! inherited voice lines so the speaker already agrees with the screen, and W9
//! measured the game framing as the only differentiator the evidence supports.
//! Do not "correct" them toward functional naming — themes are label-maps over
//! this fixed enum, and `classic` is one of them.
//!
//! The defect this module exists to make unrepresentable is v1's, and it was a
//! vocabulary rather than a bug: one enum carried scheduling, pipeline position
//! and outcome at once (W3 F146), and around it four liveness holes that are one
//! missing concept — a per-state contract (F148). Every variant here carries the
//! evidence its contract needs, so a state cannot be constructed without the
//! clock its watchdog reads.

use serde::{Deserialize, Serialize};

use crate::seq::{AttemptId, CheckpointId, PromptId, Seq, TaskId, UnitId};

/// Where a task is. Nine variants, each carrying what its contract needs.
///
/// `since` is a [`Seq`] rather than a wall-clock timestamp, so *when* and *where
/// in the replay* are the same fact (ADR-0005). v1's two NULL-clock bugs — an
/// `assignedAt IS NULL` that fails a `<` predicate by SQL three-valued logic, and
/// the same shape again on `needsHumanAt` — are unrepresentable here because the
/// field is not nullable and the variant cannot be built without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskState {
    /// standing-by. Eligible for admission once its dependency edges are
    /// satisfied — "blocked" is derived from `depends_on` rows for display and is
    /// deliberately not a variant (ADR-0004).
    Queued,
    /// orders-received. A slot is held and the workspace is locked, but no attempt
    /// has started yet. Watched on a short spin-up bound, in seconds not minutes.
    Deployed { unit: UnitId, since: Seq },
    /// engaging-target. An attempt is in flight. The liveness clock is *progress*
    /// — the `seq` of the attempt's last event — so every token batch and tool
    /// call is the heartbeat, and a task running four model calls inside one span
    /// can no longer be reaped while healthy (v1's F148 defect 2).
    Engaged { attempt: AttemptId, since: Seq },
    /// intervention-required. The slot is released at the boundary; the workspace
    /// is kept. The clock is a human one and there is **no timeout**: the watchdog
    /// distinguishes *no human yet* from *no progress*, which is the distinction
    /// F91's forty silent minutes taught the harness.
    ///
    /// 🚨 **Every exit from here is an operator's, and that is now what
    /// the table says** (F732). It used to declare a `Command::OrdersGiven` edge
    /// straight back onto a slot. Nothing in the workspace ever sent it (F703)
    /// and **no transition on the archive ever took it** — 333 transitions,
    /// 44 arrivals in this state, 41 `Abort` and 2 `Commandeer` out of it — so
    /// it is gone rather than standing as a route a reader could plan around.
    /// What exists is `abcc take` then `abcc release`, which puts the task back
    /// in `Queued` where [`crate::task::TaskState::Queued`]'s contract —
    /// *eligible for admission* — is what a fleet reads, and `abcc accept` /
    /// `abcc reject`, which end it.
    ///
    /// ⏸ The `Hold` arm out of this state has no sender either and is
    /// deliberately **left standing** (F733): unlike `OrdersGiven` it is a live
    /// command whose arm here would become reachable the day `pause` learns to
    /// address a waiting task, and whether it should is a decision, not a
    /// clean-up.
    AwaitingOrders {
        attempt: AttemptId,
        prompt: PromptId,
        since: Seq,
    },
    /// on hold. The slot is released and the work is kept at a checkpoint, so this
    /// is stop-but-keep-the-work as a *state*, reached by a transition rather than
    /// being one (W5 F138).
    Holding {
        checkpoint: CheckpointId,
        since: Seq,
    },
    /// the human has the keyboard. The workspace is held by the operator, not by
    /// the fleet. Visible, never reaped.
    Commandeered { operator_since: Seq },
    /// mission-accomplished.
    Accomplished { attempt: AttemptId },
    /// mission-failed.
    Failed { attempt: AttemptId },
    /// abort-abort.
    Aborted { reason: AbortReason, since: Seq },
}

/// Why a task was aborted. `CompletedByOperator` is the honest spelling of "the
/// human commandeered it and finished it by hand": there is no attempt to name in
/// `Accomplished`, and inventing one to satisfy the type would be exactly the
/// nullable-field-on-an-existing-variant move ADR-0004's falsifier watches for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AbortReason {
    Operator { by: String },
    CompletedByOperator { by: String },
    BudgetExhausted { which: String },
    Superseded { by: TaskId },
    Unrecoverable { detail: String },
}

/// Why a task went back to the queue. Never a bare "retry": the watchdog's two
/// bounds and the boot orphan are three different sentences to tell an operator,
/// and v1 folded all of them into a mutation of the row being retried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "requeue", rename_all = "snake_case")]
pub enum RequeueReason {
    /// `Deployed` outstayed its spin-up bound without an attempt starting.
    SpinUpTimeout { after_ms: u64 },
    /// `Engaged`'s progress clock stalled. This is the idle-gap signal, not a
    /// total-duration one (ADR-0006, F198).
    ProgressStalled { after_ms: u64 },
    /// Boot found a slot-holding state with no live worker behind it. The attempt
    /// is tombstoned rather than resumed (W3 F151).
    OrphanedByRestart,
    /// 🚨 **The attempt ended in something another attempt could plausibly
    /// fix, and the fleet has not decided whether to buy one.**
    ///
    /// F548: the driver used to land these in `Failed` while returning
    /// [`NextAction::Attempt`](crate::attempt::NextAction::Attempt), and `Failed`
    /// is terminal — so the recommendation was unreachable from the state that
    /// carried it, and the retry budget could not be spent at all. The landing is
    /// now `Queued`, which is the state whose contract is *eligible for
    /// admission*, and the fleet writes `Fail` itself once the budget is gone.
    ///
    /// The [`Why`](crate::outcome::Why) is deliberately **not** repeated here:
    /// `AttemptEnded` is written one event earlier and carries it on the
    /// [`AttemptOutcome`](crate::attempt::AttemptOutcome). Two copies of a reason
    /// are two things that can disagree.
    ///
    /// ⚠ This is the only requeue reason that is not a watchdog's or boot's,
    /// and so the only one where the task's work survives — at the closing
    /// checkpoint, which the ending took before the worktree went.
    AttemptRetryable { of: AttemptId },
}

// ---------------------------------------------------------------------------
// The per-state contract (W3 item 4, ADR-0004)
// ---------------------------------------------------------------------------

/// What a state promises about the resources it holds and the watching it gets.
///
/// The point of this type is the exhaustive `match` in [`TaskState::contract`]:
/// a tenth variant cannot be added without answering all six questions, which is
/// precisely what v1 lacked when two independent subsystems were patched to skip
/// a state in order to stay visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateContract {
    /// Does this state hold a runtime slot? Terminal states must hold nothing.
    pub holds_slot: bool,
    /// Is the task's workspace locked to it?
    pub holds_workspace: bool,
    pub liveness: Liveness,
    pub watchdog: Watchdog,
    pub boot: BootAction,
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Nothing is running and nothing is waiting; there is no clock to read.
    None,
    /// Time since the state was entered.
    SinceEntered,
    /// The `seq` of the attempt's last event. Every event the attempt writes is
    /// the heartbeat, so liveness is a query over the log rather than a second
    /// clock that can drift from the first.
    Progress,
    /// A human is expected. Measured, displayed, and never used to reap.
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watchdog {
    /// Holds nothing, so there is nothing to reclaim.
    NotWatched,
    /// Reclaimed if its clock exceeds the bound.
    Reap { bound: Bound },
    /// Watched and shown, with escalating console visibility, and **never**
    /// reaped. Every operator state is this one.
    WatchNeverReap,
}

/// Which bound a reapable state is measured against. The numbers live in
/// configuration, not in the domain type — but *which* bound applies is a
/// property of the state and belongs here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// Seconds, not minutes: a slot was granted and nothing started.
    SpinUp,
    /// The per-read idle gap. A stream silent this long is a hang, and the worker
    /// records the class rather than a generic failure (ADR-0006).
    ProgressGap,
}

/// What boot does with a task found in this state. Boot is the same code as
/// replay (ADR-0005), so this runs on every start and cannot rot the way v1's
/// second recovery path did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAction {
    /// The state survives a restart unchanged.
    Stands,
    /// The state names an in-process resource that did not survive. Return the
    /// task to `Queued`; any open attempt is tombstoned, never resumed.
    Requeue,
    /// Re-present the question to the operator and **never auto-answer it**. The
    /// arrival time is part of the contract (F132).
    RepresentPrompt,
}

impl TaskState {
    /// The contract row for this state. Exhaustive by construction.
    #[must_use]
    pub fn contract(&self) -> StateContract {
        match self {
            TaskState::Queued => StateContract {
                holds_slot: false,
                holds_workspace: false,
                liveness: Liveness::None,
                watchdog: Watchdog::NotWatched,
                boot: BootAction::Stands,
                terminal: false,
            },
            TaskState::Deployed { .. } => StateContract {
                holds_slot: true,
                holds_workspace: true,
                liveness: Liveness::SinceEntered,
                watchdog: Watchdog::Reap {
                    bound: Bound::SpinUp,
                },
                boot: BootAction::Requeue,
                terminal: false,
            },
            TaskState::Engaged { .. } => StateContract {
                holds_slot: true,
                holds_workspace: true,
                liveness: Liveness::Progress,
                watchdog: Watchdog::Reap {
                    bound: Bound::ProgressGap,
                },
                boot: BootAction::Requeue,
                terminal: false,
            },
            TaskState::AwaitingOrders { .. } => StateContract {
                holds_slot: false,
                holds_workspace: true,
                liveness: Liveness::Human,
                watchdog: Watchdog::WatchNeverReap,
                boot: BootAction::RepresentPrompt,
                terminal: false,
            },
            TaskState::Holding { .. } => StateContract {
                holds_slot: false,
                holds_workspace: true,
                liveness: Liveness::None,
                watchdog: Watchdog::WatchNeverReap,
                boot: BootAction::Stands,
                terminal: false,
            },
            TaskState::Commandeered { .. } => StateContract {
                holds_slot: false,
                holds_workspace: true,
                liveness: Liveness::Human,
                watchdog: Watchdog::WatchNeverReap,
                boot: BootAction::Stands,
                terminal: false,
            },
            TaskState::Accomplished { .. }
            | TaskState::Failed { .. }
            | TaskState::Aborted { .. } => StateContract {
                holds_slot: false,
                holds_workspace: false,
                liveness: Liveness::None,
                watchdog: Watchdog::NotWatched,
                boot: BootAction::Stands,
                terminal: true,
            },
        }
    }

    /// The variant name, for refusal messages and for the console's label map.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            TaskState::Queued => "Queued",
            TaskState::Deployed { .. } => "Deployed",
            TaskState::Engaged { .. } => "Engaged",
            TaskState::AwaitingOrders { .. } => "AwaitingOrders",
            TaskState::Holding { .. } => "Holding",
            TaskState::Commandeered { .. } => "Commandeered",
            TaskState::Accomplished { .. } => "Accomplished",
            TaskState::Failed { .. } => "Failed",
            TaskState::Aborted { .. } => "Aborted",
        }
    }

    /// The attempt this state has in flight, if any. `Accomplished` and `Failed`
    /// name the attempt that ended them, which is a record rather than a live
    /// one, so they are deliberately not included.
    #[must_use]
    pub fn attempt_in_flight(&self) -> Option<AttemptId> {
        match self {
            TaskState::Engaged { attempt, .. } | TaskState::AwaitingOrders { attempt, .. } => {
                Some(*attempt)
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.contract().terminal
    }
}

// ---------------------------------------------------------------------------
// Commands and refusals
// ---------------------------------------------------------------------------

/// What a caller may ask of a task.
///
/// ADR-0004's rule about foreign processes is the reason this type exists at all:
/// *they send commands that can be refused, never column values that cannot.*
/// There is no generic set-status anywhere in this workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Grant a slot. `Queued -> Deployed`.
    Deploy {
        unit: UnitId,
    },
    /// Start an attempt in the granted slot. `Deployed -> Engaged`.
    Engage {
        attempt: AttemptId,
    },
    /// The attempt needs the operator. `Engaged -> AwaitingOrders`, releasing the
    /// slot at the boundary.
    RequestOrders {
        attempt: AttemptId,
        prompt: PromptId,
    },
    /// Stop but keep the work, at a checkpoint. `-> Holding`.
    Hold {
        checkpoint: CheckpointId,
    },
    /// Take the work back off hold. `Holding -> Deployed`; the caller forks a new
    /// attempt from the checkpoint, because attempts are immutable.
    Resume {
        unit: UnitId,
    },
    /// The operator takes the keyboard. Legal from any non-terminal state.
    Commandeer,
    /// The operator hands the task back to the fleet. `Commandeered -> Queued`.
    Release,
    /// Return to the queue. Four callers: the watchdog's two bounds, boot's
    /// orphan sweep, and — since F548 — an attempt whose ending another attempt
    /// could plausibly fix, which lands here so that the fleet's retry budget has
    /// somewhere to be spent from. `Failed` is terminal and could not.
    Requeue {
        why: RequeueReason,
    },
    Accomplish {
        attempt: AttemptId,
    },
    Fail {
        attempt: AttemptId,
    },
    /// Legal from any non-terminal state.
    Abort {
        reason: AbortReason,
    },
}

impl Command {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Command::Deploy { .. } => "Deploy",
            Command::Engage { .. } => "Engage",
            Command::RequestOrders { .. } => "RequestOrders",
            Command::Hold { .. } => "Hold",
            Command::Resume { .. } => "Resume",
            Command::Commandeer => "Commandeer",
            Command::Release => "Release",
            Command::Requeue { .. } => "Requeue",
            Command::Accomplish { .. } => "Accomplish",
            Command::Fail { .. } => "Fail",
            Command::Abort { .. } => "Abort",
        }
    }
}

/// Why a command did not happen. A refusal is a normal outcome, not an error
/// path: the console shows it and the task is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("{command} is not legal in {state}")]
    NotLegalHere {
        command: &'static str,
        state: &'static str,
    },
    #[error("{state} is terminal; {command} would resurrect a finished task")]
    Terminal {
        command: &'static str,
        state: &'static str,
    },
    #[error("{command} named attempt {given}, but {in_flight} is the one in flight")]
    WrongAttempt {
        command: &'static str,
        given: AttemptId,
        in_flight: AttemptId,
    },
}

impl TaskState {
    /// The one transition function. Pure: it decides, and the store writes.
    ///
    /// `at` is the `Seq` of the event this transition is about to become, which
    /// is why the store allocates the seq first and asks second. That ordering is
    /// what lets every variant carry a real `since` instead of a nullable one.
    ///
    /// # Errors
    ///
    /// Returns [`Refused`] when the command is not legal in this state, when the
    /// state is terminal, or when it names an attempt that is not the one in
    /// flight.
    pub fn apply(&self, command: &Command, at: Seq) -> Result<TaskState, Refused> {
        if self.is_terminal() {
            return Err(Refused::Terminal {
                command: command.name(),
                state: self.name(),
            });
        }

        // Legal from anywhere that is not terminal, and checked before the
        // per-state table so the operator's two escape hatches never depend on
        // where the fleet happened to be.
        match command {
            Command::Abort { reason } => {
                return Ok(TaskState::Aborted {
                    reason: reason.clone(),
                    since: at,
                });
            }
            Command::Commandeer => {
                return Ok(TaskState::Commandeered { operator_since: at });
            }
            _ => {}
        }

        // Any command naming an attempt must name the one in flight. Without this
        // a stale console reply could end the wrong attempt — v1's four retry
        // mechanisms all mutated the row they were handed.
        if let (Some(named), Some(in_flight)) = (command.names_attempt(), self.attempt_in_flight())
            && named != in_flight
        {
            return Err(Refused::WrongAttempt {
                command: command.name(),
                given: named,
                in_flight,
            });
        }

        let refuse = || Refused::NotLegalHere {
            command: command.name(),
            state: self.name(),
        };

        // The arms are written one per (state, command) pair even where two of
        // them produce the same value, because this match *is* the transition
        // table and a reader has to be able to see which commands a state
        // accepts. Collapsing `Deployed | Engaged` into one arm would save a
        // line and hide the fact that both are reapable.
        #[allow(clippy::match_same_arms)]
        match (self, command) {
            (TaskState::Queued, Command::Deploy { unit }) => Ok(TaskState::Deployed {
                unit: *unit,
                since: at,
            }),

            (TaskState::Deployed { .. }, Command::Engage { attempt }) => Ok(TaskState::Engaged {
                attempt: *attempt,
                since: at,
            }),
            (TaskState::Deployed { .. }, Command::Requeue { .. }) => Ok(TaskState::Queued),

            (TaskState::Engaged { attempt, .. }, Command::RequestOrders { prompt, .. }) => {
                Ok(TaskState::AwaitingOrders {
                    attempt: *attempt,
                    prompt: *prompt,
                    since: at,
                })
            }
            (TaskState::Engaged { .. }, Command::Hold { checkpoint }) => Ok(TaskState::Holding {
                checkpoint: *checkpoint,
                since: at,
            }),
            (TaskState::Engaged { .. }, Command::Accomplish { attempt }) => {
                Ok(TaskState::Accomplished { attempt: *attempt })
            }
            (TaskState::Engaged { .. }, Command::Fail { attempt }) => {
                Ok(TaskState::Failed { attempt: *attempt })
            }
            (TaskState::Engaged { .. }, Command::Requeue { .. }) => Ok(TaskState::Queued),

            (TaskState::AwaitingOrders { .. }, Command::Hold { checkpoint }) => {
                Ok(TaskState::Holding {
                    checkpoint: *checkpoint,
                    since: at,
                })
            }

            (TaskState::Holding { .. }, Command::Resume { unit }) => Ok(TaskState::Deployed {
                unit: *unit,
                since: at,
            }),

            (TaskState::Commandeered { .. }, Command::Release) => Ok(TaskState::Queued),

            _ => Err(refuse()),
        }
    }
}

impl Command {
    /// The attempt this command names, if it names one.
    #[must_use]
    fn names_attempt(&self) -> Option<AttemptId> {
        match self {
            Command::Engage { attempt }
            | Command::RequestOrders { attempt, .. }
            | Command::Accomplish { attempt }
            | Command::Fail { attempt } => Some(*attempt),
            _ => None,
        }
    }
}
