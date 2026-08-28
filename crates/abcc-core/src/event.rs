//! The event. Everything that happens is one of these, and nothing else is a
//! source of truth.
//!
//! ADR-0005: status is a projection of this log and boot is the same code as
//! replay. ADR-0012: the console is a *reader* of it, positioned by `seq`.
//!
//! `PLAN.md` §5 names the instrumentation that must exist from the first
//! milestone rather than being added when it is wanted, and each item is a field
//! or a variant here:
//!
//! * **review minutes per merged change** — [`Event::ReviewRecorded`]. W13's
//!   ladder is measured in human review minutes and never in agent-authored
//!   commits, because every long-term failure mode in the literature is a
//!   review-burden failure. Starting the measurement at Self-Host leaves no
//!   baseline and the ladder becomes unfalsifiable.
//! * **`reasoning_tokens` on every call** — [`Usage::reasoning_tokens`].
//! * **the run's mode as its first event** — [`Event::RunStarted`].
//! * **`finish_reason == length` with an empty payload** — [`Finish::Length`]
//!   carries `content_empty`, and the consumer maps it to
//!   [`crate::outcome::Why::TruncatedAtCap`], never to a score.
//! * **every rung's `Unmeasured(Why)`** — [`Event::RungRecorded`] takes a whole
//!   [`crate::outcome::Outcome`], measured or not.

use serde::{Deserialize, Serialize};

use crate::attempt::{AttemptOutcome, Cause};
use crate::outcome::{Claim, Outcome};
use crate::run::{AttemptPhase, DowngradeReason, MissionPhase, Mode};
use crate::seq::{AttemptId, CheckpointId, MissionId, PromptId, Seq, TaskId, UnitId};
use crate::task::{Command, TaskState};

/// An event as it sits in the log: its position, its wall clock, and what
/// happened.
///
/// The ordering is `seq` and only `seq`. `at_ms` is data — it is shown to people
/// and it is never a cursor, because two cursors that must agree are two cursors
/// that will not (ADR-0012).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logged {
    pub seq: Seq,
    /// Unix milliseconds, from the host clock at the moment of the append.
    pub at_ms: i64,
    pub event: Event,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    // -- run ---------------------------------------------------------------
    /// The first event of every run, carrying the mode it was admitted in
    /// (ADR-0013). The effective mode at any point is a projection over this and
    /// [`Event::ModeDowngraded`].
    RunStarted {
        mode: Mode,
        version: String,
        pid: u32,
    },
    /// A one-way, sticky downgrade. There is no upgrade event, deliberately.
    ModeDowngraded {
        from: Mode,
        to: Mode,
        why: DowngradeReason,
    },

    // -- board -------------------------------------------------------------
    /// The mission's id is this event's own `seq`.
    MissionCreated {
        title: String,
    },
    /// The task's id is this event's own `seq`.
    TaskCreated {
        mission: MissionId,
        title: String,
        prompt: String,
    },
    /// An edge in the dependency graph. `Blocked` is derived from these rows for
    /// display and is deliberately not a lifecycle variant.
    TaskDependsOn {
        task: TaskId,
        on: TaskId,
    },
    MissionPhaseEntered {
        mission: MissionId,
        phase: MissionPhase,
    },

    // -- lifecycle ---------------------------------------------------------
    /// The only event that moves a task. Written by the single transition writer,
    /// in the same transaction as the status projection it produces.
    ///
    /// It carries `from` as well as `to` so that a reader tailing from the middle
    /// of the log never has to have seen the earlier events to render a
    /// transition, and so a replay can assert that it lands where the original
    /// did.
    TaskTransitioned {
        task: TaskId,
        command: Command,
        from: TaskState,
        to: TaskState,
    },
    /// A command the writer refused. Recorded because a refusal is a normal
    /// outcome the operator should be able to see, and because a foreign process
    /// hammering a refused command is a thing worth being able to notice.
    CommandRefused {
        task: TaskId,
        command: Command,
        state: TaskState,
        refusal: String,
    },

    // -- attempts ----------------------------------------------------------
    /// The attempt's id is this event's own `seq`.
    AttemptStarted {
        task: TaskId,
        unit: UnitId,
        cause: Cause,
        checkpoint_from: Option<CheckpointId>,
    },
    AttemptEnded {
        task: TaskId,
        attempt: AttemptId,
        outcome: AttemptOutcome,
    },
    AttemptPhaseEntered {
        attempt: AttemptId,
        phase: AttemptPhase,
    },

    // -- the model ---------------------------------------------------------
    ModelCallStarted {
        attempt: AttemptId,
        provider: String,
        model: String,
        /// Which frozen tool head this call used. The set is enumerated and frozen
        /// per attempt (ADR-0011) because prefix caching saves 79.7% of TTFT and
        /// one token changed at the *front* costs a full cold prompt.
        head: String,
        /// Tokens the request declared it would accept back.
        budget: u32,
    },
    ModelCallEnded {
        attempt: AttemptId,
        usage: Usage,
        finish: Finish,
        /// Time to first byte, milliseconds. The number the per-read timeout is
        /// set against.
        ttfb_ms: u64,
        elapsed_ms: u64,
    },

    // -- tools -------------------------------------------------------------
    ToolCallStarted {
        attempt: AttemptId,
        tool: String,
        /// The permission tier this call was admitted at. Security is denying the
        /// class (ADR-0014): a tier is removed from a role, not argued with.
        tier: String,
    },
    ToolCallEnded {
        attempt: AttemptId,
        tool: String,
        exit: Option<i32>,
        elapsed_ms: u64,
        /// Set when the tool produced no measurable ending — the class, never a
        /// generic failure.
        unmeasured: Option<crate::outcome::Why>,
    },

    // -- the gate ----------------------------------------------------------
    /// Every rung, measured or not (ADR-0009 §7). This is what makes the report a
    /// projection of the log rather than a second source of truth.
    RungRecorded {
        attempt: AttemptId,
        outcome: Outcome,
    },
    /// What the model said about its own work. Shown to the operator, and there
    /// is no path from here to an [`Outcome`].
    ClaimRecorded {
        attempt: AttemptId,
        claim: Claim,
    },

    // -- isolation ---------------------------------------------------------
    /// A temp-index snapshot, written to a ref. The checkpoint's id is this
    /// event's own `seq`; the sha is the identifier, and `git_ref` is what stops
    /// `git gc --prune=now` from collecting it (ADR-0007, F330).
    CheckpointTaken {
        task: TaskId,
        sha: String,
        git_ref: String,
    },
    WorktreeOpened {
        task: TaskId,
        path: String,
        sha: String,
    },
    WorktreeClosed {
        task: TaskId,
        path: String,
    },

    // -- the operator ------------------------------------------------------
    /// A question put to the operator. The prompt's id is this event's own `seq`,
    /// and boot re-presents it rather than answering it.
    OperatorPrompted {
        task: TaskId,
        attempt: AttemptId,
        question: String,
    },
    OperatorAnswered {
        task: TaskId,
        prompt: PromptId,
        answer: String,
    },
    /// A control verb arriving from the console. It is written here first and the
    /// in-memory poke is a latency optimisation, never the truth (ADR-0006).
    ControlRequested {
        task: TaskId,
        control: Control,
    },
    /// The worker acknowledging it at a step boundary.
    ControlApplied {
        task: TaskId,
        control: Control,
    },

    // -- instrumentation ---------------------------------------------------
    /// Proof the run is alive, for the bar that says no gap over 10 seconds goes
    /// unmarked (ADR-0012 §5). The harness once sat silent for 40 minutes, 240x
    /// the attention limit, and could not tell that from a hang.
    LivenessMark {
        attempt: AttemptId,
        note: String,
    },
    /// 🚨 The measurement W13's whole ladder is defined in. One column and one
    /// event type, trivial now and unrecoverable later.
    ReviewRecorded {
        /// What was reviewed — a commit sha, or a task id.
        change: String,
        /// Stored in **seconds**, reported in minutes. The ladder's unit is
        /// minutes, but a float in a durable record is a quantity that stops
        /// summing exactly the moment there are enough of them to matter — and
        /// this is a record whose whole purpose is to be summed over months.
        seconds: u32,
        by: String,
        /// Whether the change crossed a module boundary. M3 requires ten
        /// consecutive boundary-crossing gate-accepted tasks, and v1 already
        /// occupies M0 with 15 of 471 commits, none architectural.
        crossed_boundary: bool,
    },
    /// Anything worth seeing that is not one of the above. Deliberately last and
    /// deliberately dull: a note is not a state, and nothing may branch on it.
    Note {
        text: String,
    },
}

/// What a model call cost. `reasoning_tokens` is logged on every call from day
/// one (`PLAN.md` §5) — it is `None` when the provider did not report it, which
/// is a different thing from zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub reasoning_tokens: Option<u32>,
    /// Prefix-cache hits, where the server reports them. This is the number that
    /// tells you whether the frozen tool head is doing its job.
    pub cached_tokens: Option<u32>,
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "finish", rename_all = "snake_case")]
pub enum Finish {
    Stop,
    /// Hit the token cap. 🚨 `content_empty` is the field that matters: an empty
    /// payload at the cap is `Uncertain` and never a score.
    Length {
        content_empty: bool,
    },
    ToolCalls,
    /// The stream ended without a finish reason — a dropped socket, or a proxy
    /// that returned 200 and then nothing.
    Truncated {
        detail: String,
    },
}

impl Finish {
    /// Whether this ending is an absence rather than an answer.
    #[must_use]
    pub fn is_uncertain(&self) -> bool {
        matches!(
            self,
            Finish::Length {
                content_empty: true
            } | Finish::Truncated { .. }
        )
    }
}

/// The operator's control verbs, as they cross the control channel.
///
/// ADR-0012 §4: eight verbs reduce to three mechanisms, and this is the first —
/// a control channel with a step boundary the worker checks. The unit of control
/// is the **task**, because the unit of control must be the unit that holds
/// resources: every verb ends in *"and then what happens to its model slot and
/// its workspace lock?"*, and only a task can answer that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "control", rename_all = "snake_case")]
pub enum Control {
    /// Stop at the next step boundary and keep the work at a checkpoint.
    Pause,
    /// Stop now and keep the work, discarding only the un-checkpointed
    /// generation.
    Halt,
    /// Stop now and keep nothing.
    Kill,
    /// Change the question and fork a new attempt from the checkpoint.
    Redirect { prompt: String },
    /// Resume from `Holding`.
    Resume,
}

impl Event {
    /// The task this event is about, where it has one. This is the indexed
    /// column, so a reader can page one task's history without scanning.
    #[must_use]
    pub fn task(&self) -> Option<TaskId> {
        match self {
            Event::TaskDependsOn { task, .. }
            | Event::TaskTransitioned { task, .. }
            | Event::CommandRefused { task, .. }
            | Event::AttemptStarted { task, .. }
            | Event::AttemptEnded { task, .. }
            | Event::CheckpointTaken { task, .. }
            | Event::WorktreeOpened { task, .. }
            | Event::WorktreeClosed { task, .. }
            | Event::OperatorPrompted { task, .. }
            | Event::OperatorAnswered { task, .. }
            | Event::ControlRequested { task, .. }
            | Event::ControlApplied { task, .. } => Some(*task),
            _ => None,
        }
    }

    /// The attempt this event is about, where it has one.
    #[must_use]
    pub fn attempt(&self) -> Option<AttemptId> {
        match self {
            Event::AttemptEnded { attempt, .. }
            | Event::AttemptPhaseEntered { attempt, .. }
            | Event::ModelCallStarted { attempt, .. }
            | Event::ModelCallEnded { attempt, .. }
            | Event::ToolCallStarted { attempt, .. }
            | Event::ToolCallEnded { attempt, .. }
            | Event::RungRecorded { attempt, .. }
            | Event::ClaimRecorded { attempt, .. }
            | Event::OperatorPrompted { attempt, .. }
            | Event::LivenessMark { attempt, .. } => Some(*attempt),
            _ => None,
        }
    }

    /// The discriminant, stored as its own column so the common queries — the
    /// console's filters, the liveness sweep, the review ladder — are index scans
    /// rather than JSON extraction over every row.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Event::RunStarted { .. } => "run_started",
            Event::ModeDowngraded { .. } => "mode_downgraded",
            Event::MissionCreated { .. } => "mission_created",
            Event::TaskCreated { .. } => "task_created",
            Event::TaskDependsOn { .. } => "task_depends_on",
            Event::MissionPhaseEntered { .. } => "mission_phase_entered",
            Event::TaskTransitioned { .. } => "task_transitioned",
            Event::CommandRefused { .. } => "command_refused",
            Event::AttemptStarted { .. } => "attempt_started",
            Event::AttemptEnded { .. } => "attempt_ended",
            Event::AttemptPhaseEntered { .. } => "attempt_phase_entered",
            Event::ModelCallStarted { .. } => "model_call_started",
            Event::ModelCallEnded { .. } => "model_call_ended",
            Event::ToolCallStarted { .. } => "tool_call_started",
            Event::ToolCallEnded { .. } => "tool_call_ended",
            Event::RungRecorded { .. } => "rung_recorded",
            Event::ClaimRecorded { .. } => "claim_recorded",
            Event::CheckpointTaken { .. } => "checkpoint_taken",
            Event::WorktreeOpened { .. } => "worktree_opened",
            Event::WorktreeClosed { .. } => "worktree_closed",
            Event::OperatorPrompted { .. } => "operator_prompted",
            Event::OperatorAnswered { .. } => "operator_answered",
            Event::ControlRequested { .. } => "control_requested",
            Event::ControlApplied { .. } => "control_applied",
            Event::LivenessMark { .. } => "liveness_mark",
            Event::ReviewRecorded { .. } => "review_recorded",
            Event::Note { .. } => "note",
        }
    }
}
