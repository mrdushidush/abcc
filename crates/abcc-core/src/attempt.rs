//! Attempts, which are immutable, and the lineage that makes them so.
//!
//! ADR-0004. v1 has four retry mechanisms and no lineage, because **every retry
//! mutates the row it retries** (F150), so *what did the previous attempt do* is
//! unanswerable after the fact. Here retry, edit-prompt, re-route and replay are
//! one operation — fork from a checkpoint with a [`Cause`] — and lineage exists by
//! construction rather than by discipline: it cannot be lost, because an attempt
//! cannot be mutated.

use serde::{Deserialize, Serialize};

use crate::outcome::Why;
use crate::seq::{AttemptId, CheckpointId, Seq, TaskId};

/// Why this attempt exists. The four verbs an operator thinks of as different
/// things are one mechanism wearing four labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum Cause {
    /// The first attempt at this task.
    Fresh,
    /// The same prompt again. Budget 2 (ADR-0010): the only purchase the
    /// architecture offers is another attempt, never another tier.
    Retry { of: AttemptId },
    /// The task was narrowed or widened after the operator read what happened.
    Rescope { of: AttemptId },
    /// The operator edited the prompt.
    Edit { of: AttemptId },
    /// Re-run for the console's after-action view. Produces no new work.
    Replay { of: AttemptId },
}

impl Cause {
    /// The attempt this one forked from, if any.
    #[must_use]
    pub fn parent(&self) -> Option<AttemptId> {
        match self {
            Cause::Fresh => None,
            Cause::Retry { of }
            | Cause::Rescope { of }
            | Cause::Edit { of }
            | Cause::Replay { of } => Some(*of),
        }
    }

    /// Whether this attempt counts against the retry budget. A rescope or an edit
    /// is the operator changing the question, so it does not — v1 folded all four
    /// into one counter and then could not explain the number.
    #[must_use]
    pub fn spends_retry_budget(&self) -> bool {
        matches!(self, Cause::Retry { .. })
    }
}

/// How an attempt ended. Four cases, and `Uncertain` is a real one rather than a
/// value that quietly sets `success = true` in the same literal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// The work was done and the gate measured it.
    Success,
    /// It failed in a way another attempt could plausibly fix.
    SoftFailure { why: Why },
    /// It failed in a way another attempt would repeat.
    HardFailure { why: Why },
    /// Neither. The commonest member is the model stopping at the token cap with
    /// an empty payload, which is an absence and not a score.
    Uncertain { why: Why },
    /// 🚨 **A deterministic rung refused. This is a measurement, not an
    /// absence** — the host watched a checker run and watched it say no.
    ///
    /// It carries **no [`Why`]**, and that is the point. `Why` is the vocabulary
    /// for a rung that produced *no* measurement: every one of its variants is a
    /// sentence about something that did not happen, and putting a refusal in
    /// there would put a failure into the enum whose whole job is to keep
    /// failures and absences apart. So this carries what
    /// [`Headline::Red`](crate::outcome::Headline::Red) carries — the rung's
    /// name and the evidence — and `Success` is the precedent: the two
    /// *definite* endings are the two that need no `Why`.
    ///
    /// ⚠ **Only a deterministic rung can put an attempt here.**
    /// `AttemptPhase::may_refuse` is where that is said as code, and a model's
    /// verdict is a `Claim`, which has no path to an `Outcome` and therefore
    /// none to here (ADR-0009 §4, David 2026-08-28).
    Refused { rung: String, detail: String },
}

impl AttemptOutcome {
    /// Whether the fleet may spend another attempt on this. Note that `Uncertain`
    /// is retryable and `HardFailure` is not, which is the distinction v1's single
    /// boolean could not carry.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AttemptOutcome::SoftFailure { .. }
                | AttemptOutcome::Uncertain { .. }
                // A refusal is the case another attempt most plainly could fix:
                // the gate said what is wrong and where. ⚠ Retryable is not the
                // same as *worth retrying* — the budget is 2 and it is the
                // fleet's, not this function's.
                | AttemptOutcome::Refused { .. }
        )
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            AttemptOutcome::Success => "Success",
            AttemptOutcome::SoftFailure { .. } => "SoftFailure",
            AttemptOutcome::HardFailure { .. } => "HardFailure",
            AttemptOutcome::Uncertain { .. } => "Uncertain",
            AttemptOutcome::Refused { .. } => "Refused",
        }
    }
}

/// An attempt as the store holds it. Every field except `ended` and `outcome` is
/// written once, at `AttemptStarted`, and those two are written once at
/// `AttemptEnded`. Nothing else ever updates this row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: AttemptId,
    pub task: TaskId,
    /// The tree this attempt started from. `None` only for an attempt on a task
    /// whose workspace was untouched.
    pub checkpoint_from: Option<CheckpointId>,
    pub cause: Cause,
    pub started: Seq,
    pub ended: Option<Seq>,
    pub outcome: Option<AttemptOutcome>,
}

impl Attempt {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.ended.is_none()
    }
}

/// What the fleet does next with a task, after an attempt ends.
///
/// ADR-0010, and every item of W4 inverted its brief to get here: **no
/// pre-dispatch estimate, one tier, retry budget 2.** There is no `Escalate`,
/// because there is no second tier to escalate to — the only purchase available
/// is another attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "next", rename_all = "snake_case")]
pub enum NextAction {
    /// Fork another attempt.
    Attempt { cause: Cause },
    /// Nothing more to try; the task takes its terminal state.
    Stop,
    /// A durable state, not a log line: the task waits for a human and is watched
    /// but never reaped.
    HandToOperator { question: String },
}
