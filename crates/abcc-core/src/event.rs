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
use crate::redact::Scrubbed;
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
        /// 🚨 **The ceiling that was actually in force** — the slot's cap, or the
        /// role's own, whichever was narrower (ADR-0014 §4).
        ///
        /// It is here because a slot cap changes what the model is *told* it
        /// has, and without it a run under a cap and the same run without one
        /// would differ in the prompt and agree in every event. Status is a
        /// projection of the log alone, so a fact that reaches only the prompt
        /// is a fact the log cannot reconstruct.
        ///
        /// ⚠ `#[serde(default)]` for the logs written before the cap existed,
        /// where it reads as the empty string — *not recorded*, which is a
        /// different thing from *no ceiling*.
        #[serde(default)]
        ceiling: String,
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
        /// 🚨 **F511: where the completion actually went.**
        ///
        /// [`Usage::completion_tokens`] is a total, and a total is the one thing
        /// it is not safe to reason from here. Five live turns spent 94–98% of
        /// their budget assembling **one enormous tool call** and were cut
        /// mid-argument; from the usage block alone that is indistinguishable
        /// from a model writing an essay instead of calling a tool — and reading
        /// the totals got exactly that backwards once, because the stream counts
        /// text and reasoning separately and counts **tool-call arguments as
        /// neither**.
        ///
        /// ⚠ The answer was recoverable only by cross-reading
        /// [`Event::LivenessMark`], which samples on a timer and so lands near
        /// the end of a turn by luck rather than by construction. This field is
        /// the same fact, measured on purpose.
        ///
        /// 🚨 **`Option`, and defaulted, because BOOT IS REPLAY.** The log is
        /// durable and this enum is its schema: a required field added here
        /// makes every event already on disk undeserializable, and the first
        /// live run after this field was added refused to start with *missing
        /// field `composition`* over 1,200 existing rows. ⚠ And it is `Option`
        /// rather than a zeroed default for the same reason
        /// [`Usage::reasoning_tokens`] is: **`None` means nobody counted, which
        /// is not the same as counted zero** — a defaulted `Composition` would
        /// say every historical turn produced no text, no trace and no calls,
        /// which is a false statement the log would then repeat forever.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        composition: Option<Composition>,
    },
    /// 🚨 **F513: the phase's own accounting, which used to exist only on a
    /// terminal.**
    ///
    /// The engine's `PhaseReport` was built on every ending, printed by
    /// `abcc run`, and never written down — so ADR-0010 §7's *recording is what
    /// produces the population* was not happening. [`TraceSignal`] is computed
    /// **in flight** from the stream's deltas and cannot be reconstructed from
    /// any other event: two `OpenAt200` observations exist in this project's
    /// whole history and both survive only because a person read them off stdout
    /// before the scrollback went.
    ///
    /// The counts *are* derivable by folding the raw events. They are here
    /// anyway, because an accounting split across two mechanisms is an
    /// accounting nobody checks.
    PhaseEnded {
        attempt: AttemptId,
        /// The head's call sign, so the feed names who spent this. The phase is
        /// deliberately not repeated here: it is the last
        /// [`Event::AttemptPhaseEntered`] before this one, and a second copy is a
        /// second thing that can disagree. Same shape as [`Event::PhaseNudged`].
        by: String,
        turns: u32,
        tool_calls: u32,
        denials: u32,
        prompt_tokens: u32,
        completion_tokens: u32,
        /// `None` when no call in the phase reported one, which is different
        /// from zero.
        reasoning_tokens: Option<u32>,
        trace: TraceSignal,
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
        /// 🚨 **F505: the arguments, and only when the call failed.**
        ///
        /// Five consecutive `apply_patch` refusals were once undiagnosable after
        /// the fact, because the log carried the tool, the tier and the class
        /// and not the thing that was refused — so the only way to see the patch
        /// was to make the model produce it again. A refusal nobody can read is
        /// a refusal nobody can fix.
        ///
        /// ⚠ `None` on success, deliberately: an argument string that is already
        /// reflected in the tree is a copy of the work, and the log is not a
        /// second copy of the workspace. ⚠ It is **not** rendered on the feed —
        /// see F501; one event is one line, and this one can be a whole diff.
        ///
        /// 🚨 **[`Scrubbed`] rather than `String`, and that is the enforcement**
        /// (ADR-0014 §5). The arguments of a refused `bash` call are an operator's
        /// shell line and the arguments of a refused `write_file` are a file's
        /// whole content; this is the field that puts either on disk verbatim, so
        /// it is the field that may not take text nobody scrubbed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<Scrubbed>,
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
    /// 🚨 **F503: the model produced no answer and was asked again.** A repair,
    /// not an ending — the ending is [`Why::SaidNothing`] and it only arrives
    /// when the nudges run out.
    ///
    /// It is on the log because it changes what the model was shown. Across
    /// seven runs of one task the closing answer was missing **5 times**, and a
    /// repair that frequent, left untraced, would make every later transcript a
    /// record of a conversation nobody can reconstruct.
    ///
    /// [`Why::SaidNothing`]: crate::outcome::Why::SaidNothing
    PhaseNudged {
        attempt: AttemptId,
        /// The head's call sign, so the feed names who went quiet.
        by: String,
        /// Nudges remaining after this one.
        left: u8,
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
    /// 🚨 **The one control no donor in the family has** (ADR-0014 §6, F420).
    ///
    /// The weights are the ungated input: every other dependency passes a
    /// supply-chain gate and the model does not. The control is *record the
    /// digest at first pull and compare it on every start, surfacing a mismatch
    /// as a run-visible event* — this is that event.
    ///
    /// ⚠ **Measured 2026-09-11, and it is why there are two positive arms.**
    /// Neither `/v1/models` nor LM Studio's `/api/v0/models` carries a digest, a
    /// size or a path — the listing identifies a model by *name*, which is the
    /// thing being checked. So the digest is of the file, and a full SHA-256 of
    /// the champion's 12.67 GiB costs **51.9 s**. Paying that on every start
    /// would get the check turned off, so a start compares the cheap identity
    /// and [`WeightsOutcome`] keeps the weaker answer from reading as the
    /// stronger one.
    WeightsChecked {
        /// The model the run asked for.
        model: String,
        /// The digest that identifies the bytes — `None` only when there was no
        /// file to read.
        digest: Option<String>,
        outcome: WeightsOutcome,
    },
    /// Anything worth seeing that is not one of the above. Deliberately last and
    /// deliberately dull: a note is not a state, and nothing may branch on it.
    Note {
        text: String,
    },
}

/// What comparing the weights against their pin found.
///
/// 🚨 **`Verified` and `Unchanged` are separate arms and that is the whole
/// design.** This is F495's shape one asset over: there, keeping *the server
/// says it is loaded* apart from *the server lists it* is what stops a weaker
/// answer being read as a stronger one. Here, `Verified` means the 51.9-second
/// read happened and the bytes hash to the pin; `Unchanged` means the length and
/// the modification time match, so **the digest was not recomputed**. One is a
/// measurement of the bytes and the other is a measurement of the directory
/// entry, and collapsing them would make every start look like a full check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "weights", rename_all = "snake_case")]
pub enum WeightsOutcome {
    /// First sight of this model. The digest was computed and is now the
    /// reference. ⚠ **A first pin trusts what is there** — it can only record
    /// the bytes, never vouch for them — which is why the README says where the
    /// weights came from is the operator's assertion.
    Pinned,
    /// The digest was recomputed from the file and matches the pin.
    Verified,
    /// Length and modification time match the pin, so the file was not read.
    /// Cheap, and weaker on purpose: it catches a re-pull or a swapped file and
    /// it does not catch an adversary who preserves both.
    Unchanged,
    /// 🚨 The bytes behind this model id are not the bytes that were pinned.
    Changed {
        /// The digest that was pinned, so the operator can tell a re-download
        /// from something else.
        was: String,
    },
    /// There was nothing to check, and why. Not a pass: a control that cannot
    /// find its subject has to say so rather than stay quiet.
    Unlocated { why: String },
}

/// 🚨 **F511: what a completion was made OF, as opposed to how big it was.**
///
/// The three parts are counted separately because the stream produces them
/// separately and because **only their sum is reported by the server**. A turn
/// at the cap with 8,192 completion tokens, ~400 of them reasoning, looks like
/// 7,700 tokens of prose in the usage block; in five live turns it was 7,700
/// tokens of **one tool call's arguments**, which is the opposite failure and
/// has the opposite fix.
///
/// ⚠ Characters, not tokens — this is counted at the seam where characters are
/// what exist. A ratio of roughly four to one holds for this stack, and the
/// point of the field is the *split*, not a second token count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Composition {
    pub text_chars: u32,
    /// The trace's size, never its content: it is the quantity that overran, and
    /// it is not evidence of anything a person would want to read back.
    pub reasoning_chars: u32,
    /// One entry per tool call the turn asked for, in the order they arrived.
    pub calls: Vec<CallShape>,
}

/// One tool call a turn asked for, sized — and, when the turn was thrown away,
/// kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallShape {
    pub tool: String,
    pub argument_chars: u32,
    /// 🚨 **The arguments, and only when this turn's payload was DISCARDED.**
    ///
    /// Same rule as [`Event::ToolCallEnded`]'s `arguments` (F505): keep the text
    /// exactly when the log is the only copy of it. A turn cut at the cap never
    /// runs its calls (F506), so nothing else in the log or the tree will ever
    /// say what was being written — and the front of a cut argument is where the
    /// path is, which is the whole diagnostic.
    ///
    /// ⚠ `None` for every turn that was used, deliberately: a call that ran has
    /// its arguments on [`Event::ToolCallEnded`] if it failed, and its effect in
    /// the tree if it did not. ⚠ Bounded by the head's own budget, because a
    /// discarded payload is by definition at most one completion. ⚠ Not rendered
    /// on the feed — F501, one event is one line, and this one can be a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// ADR-0010 §7's one in-flight signal: **at token 200, has the reasoning trace
/// closed?**
///
/// 🚨 **It is a stop, not a score.** It feeds `Uncertain` and the next action,
/// and it never becomes a number displayed next to an answer — where a human
/// wants a confidence number, they get the verifier's result, which is 547 ms
/// and checkable.
///
/// ⚠ It lives here, in the log's vocabulary, because it is **computed in flight
/// and derivable from nothing else**: by the time a turn has ended, the deltas
/// that would answer the question are gone. A signal the log cannot hold is a
/// signal whose population never gets built (F513).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "trace", rename_all = "snake_case")]
pub enum TraceSignal {
    /// The provider reported no reasoning trace at all. Not the same as a trace
    /// of length zero.
    Absent,
    /// The trace had closed by the time the turn reached 200 completion tokens.
    Closed,
    /// It had not. This turn is far likelier to end at the ceiling with nothing.
    OpenAt200,
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
            | Event::PhaseNudged { attempt, .. }
            | Event::PhaseEnded { attempt, .. }
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
            Event::PhaseNudged { .. } => "phase_nudged",
            Event::PhaseEnded { .. } => "phase_ended",
            Event::ReviewRecorded { .. } => "review_recorded",
            Event::WeightsChecked { .. } => "weights_checked",
            Event::Note { .. } => "note",
        }
    }
}
