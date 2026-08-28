//! The label map, and the only place the domain vocabulary meets a screen.
//!
//! ADR-0012 §6: **a theme is a map from enum variant to displayed string,
//! exhaustive over the enum**, with `classic` as the identity map. The enum
//! itself never moves — W9 scanned 340 comparable projects and found game, RTS,
//! battle, isometric and sprite vocabulary in exactly none of them, which makes
//! the framing the only differentiator the evidence supports and therefore the
//! last thing to cut. So the off switch is cosmetic, and the ADR says what that
//! costs in its own words: **logs and field names stay military even with the
//! theme off.** `classic` shows a state under its own variant name, which is as
//! functional as `Deployed` gets.
//!
//! The exhaustive `match` is the whole mechanism. A tenth [`TaskState`] cannot
//! ship without a label in every theme, because the compiler will not build one
//! that lacks it.

use abcc_core::run::{AttemptPhase, MissionPhase, Mode};
use abcc_core::task::TaskState;

/// Which label map the console is speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// The voice the states were named against. Default, because the naming was
    /// chosen against 96 inherited voice lines so the speaker already agrees with
    /// the screen.
    #[default]
    Command,
    /// The identity map: every variant under its own name.
    Classic,
}

impl Theme {
    /// Every theme, so a test can drive all of them over all of an enum.
    pub const ALL: [Theme; 2] = [Theme::Command, Theme::Classic];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Theme::Command => "command",
            Theme::Classic => "classic",
        }
    }

    /// A task's state.
    #[must_use]
    pub fn state(self, state: &TaskState) -> &'static str {
        match self {
            Theme::Command => match state {
                TaskState::Queued => "STANDING BY",
                TaskState::Deployed { .. } => "ORDERS RECEIVED",
                TaskState::Engaged { .. } => "ENGAGING TARGET",
                TaskState::AwaitingOrders { .. } => "INTERVENTION REQUIRED",
                TaskState::Holding { .. } => "ON HOLD",
                TaskState::Commandeered { .. } => "UNDER MANUAL CONTROL",
                TaskState::Accomplished { .. } => "MISSION ACCOMPLISHED",
                TaskState::Failed { .. } => "MISSION FAILED",
                TaskState::Aborted { .. } => "ABORT",
            },
            Theme::Classic => match state {
                TaskState::Queued => "Queued",
                TaskState::Deployed { .. } => "Deployed",
                TaskState::Engaged { .. } => "Engaged",
                TaskState::AwaitingOrders { .. } => "AwaitingOrders",
                TaskState::Holding { .. } => "Holding",
                TaskState::Commandeered { .. } => "Commandeered",
                TaskState::Accomplished { .. } => "Accomplished",
                TaskState::Failed { .. } => "Failed",
                TaskState::Aborted { .. } => "Aborted",
            },
        }
    }

    /// An attempt-level phase (ADR-0002).
    #[must_use]
    pub fn attempt_phase(self, phase: AttemptPhase) -> &'static str {
        match self {
            Theme::Command => match phase {
                AttemptPhase::Localize => "RECON",
                AttemptPhase::Change => "ASSAULT",
                AttemptPhase::Measure => "ASSESS",
                AttemptPhase::Judge => "DEBRIEF",
                AttemptPhase::Veto => "STAND DOWN",
            },
            Theme::Classic => match phase {
                AttemptPhase::Localize => "Localize",
                AttemptPhase::Change => "Change",
                AttemptPhase::Measure => "Measure",
                AttemptPhase::Judge => "Judge",
                AttemptPhase::Veto => "Veto",
            },
        }
    }

    /// A mission-level phase (ADR-0002).
    #[must_use]
    pub fn mission_phase(self, phase: MissionPhase) -> &'static str {
        match self {
            Theme::Command => match phase {
                MissionPhase::Plan => "BRIEFING",
                MissionPhase::Execute => "OPERATION",
                MissionPhase::Integrate => "REGROUP",
                MissionPhase::Accept => "SIGN-OFF",
            },
            Theme::Classic => match phase {
                MissionPhase::Plan => "Plan",
                MissionPhase::Execute => "Execute",
                MissionPhase::Integrate => "Integrate",
                MissionPhase::Accept => "Accept",
            },
        }
    }

    /// The run's mode (ADR-0013). Two values, not three.
    #[must_use]
    pub fn mode(self, mode: Mode) -> &'static str {
        match self {
            Theme::Command => match mode {
                Mode::SinglePlayer => "SINGLE PLAYER",
                Mode::CoOp => "CO-OP",
            },
            Theme::Classic => match mode {
                Mode::SinglePlayer => "SinglePlayer",
                Mode::CoOp => "CoOp",
            },
        }
    }
}
