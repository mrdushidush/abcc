//! Run-level vocabulary: the mode, and the two levels of phase.
//!
//! ADR-0013 for the mode, ADR-0002 for the phases.

use serde::{Deserialize, Serialize};

/// What a run is allowed to talk to. **Two values, not three** (ADR-0013): they
/// differ only in whether a `Cloud` provider is admitted, which makes
/// `SinglePlayer` a policy rather than a degraded code path — and that is the
/// whole reason it can be the primary mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Zero `Cloud` providers admitted.
    SinglePlayer,
    /// A `Cloud` provider may be admitted, subject to the run's frozen egress
    /// policy.
    CoOp,
}

impl Mode {
    /// Mode is a **one-way sticky ratchet**. `CoOp -> SinglePlayer` is permitted
    /// on a budget ceiling, an egress denial or a network failure, and latches
    /// for the rest of the run. The reverse is denied: an upgrade mid-run would
    /// make the run's own egress record retroactively wrong.
    #[must_use]
    pub fn may_move_to(self, to: Mode) -> bool {
        matches!((self, to), (Mode::CoOp, Mode::SinglePlayer))
    }
}

/// Why a run gave up its cloud half. Carried on the downgrade event so the
/// console can show it and replay reproduces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "downgrade", rename_all = "snake_case")]
pub enum DowngradeReason {
    BudgetCeiling { which: String },
    EgressDenied { rule: String },
    NetworkFailure { detail: String },
    Operator { by: String },
}

/// The mission-level phases. `Plan` and `Accept` bracket the task set; `Integrate`
/// is the one that has no model in it at all — it builds and tests the *assembled*
/// workspace, which is the only place the tasks meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionPhase {
    /// Model, read-only.
    Plan,
    /// The task set runs.
    Execute,
    /// No model. Build and test the assembled workspace.
    Integrate,
    /// Human by default; a deterministic ladder rung when unattended.
    Accept,
}

/// The attempt-level phases. BCF's nine stages collapse to three, because six of
/// them existed only to compensate for having no repository (W11).
///
/// Router, Tester and CTO are deliberately **not** phases: a phase is the role a
/// slot plays for one call (ADR-0003), which is why adding a phase costs nothing
/// and adding a unit type costs a console portrait that can never be lit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    /// Find the place. Model, read-only.
    Localize,
    /// Make the change. Model, with tools.
    Change,
    /// **No model.** Deterministic, tri-state, and the thing the Judge is shown.
    Measure,
    /// One model call, no tools, and it **sees the measurements**. Pairwise
    /// against the pre-image rather than a pointwise score.
    ///
    /// 🚨 It reports. It never blocks — only deterministic rungs may refuse
    /// (ADR-0009 §4, David 2026-08-28).
    Judge,
    /// Deterministic, non-scoring refusal.
    Veto,
}

impl AttemptPhase {
    /// Whether this phase makes a model call at all. `Measure` and `Veto` do not,
    /// which is what makes them the only phases allowed to refuse.
    #[must_use]
    pub fn uses_model(self) -> bool {
        match self {
            AttemptPhase::Localize | AttemptPhase::Change | AttemptPhase::Judge => true,
            AttemptPhase::Measure | AttemptPhase::Veto => false,
        }
    }

    /// Whether an unattended `Accept` may be blocked by this phase.
    ///
    /// This is David's ruling of 2026-08-28 held strictly rather than nearly, and
    /// it is stated as code here so that wiring the Judge into a gate becomes a
    /// visible edit rather than a quiet one. There is no false-fail rate to tune,
    /// because a false fail is a bug in a rung.
    #[must_use]
    pub fn may_refuse(self) -> bool {
        !self.uses_model()
    }
}
