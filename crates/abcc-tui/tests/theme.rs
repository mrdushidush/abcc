//! The label map is exhaustive over the enum, and the enum does not move.
//!
//! ADR-0012 §6. The compiler enforces the exhaustiveness — these tests check the
//! two things it cannot: that a theme is a *relabelling* rather than a rename, and
//! that `classic` really is the identity map it claims to be.

use abcc_core::attempt::Cause;
use abcc_core::run::{AttemptPhase, MissionPhase, Mode};
use abcc_core::seq::{AttemptId, CheckpointId, PromptId, Seq, TaskId, UnitId};
use abcc_core::task::{AbortReason, TaskState};
use abcc_tui::Theme;

/// One of every state. Written out rather than generated, because the point of
/// the list is that adding a tenth variant makes this file fail to compile.
fn every_state() -> Vec<TaskState> {
    let seq = Seq::new(7);
    vec![
        TaskState::Queued,
        TaskState::Deployed {
            unit: UnitId(0),
            since: seq,
        },
        TaskState::Engaged {
            attempt: AttemptId::at(seq),
            since: seq,
        },
        TaskState::AwaitingOrders {
            attempt: AttemptId::at(seq),
            prompt: PromptId::at(seq),
            since: seq,
        },
        TaskState::Holding {
            checkpoint: CheckpointId::at(seq),
            since: seq,
        },
        TaskState::Commandeered {
            operator_since: seq,
        },
        TaskState::Accomplished {
            attempt: AttemptId::at(seq),
        },
        TaskState::Failed {
            attempt: AttemptId::at(seq),
        },
        TaskState::Aborted {
            reason: AbortReason::Operator {
                by: "david".to_string(),
            },
            since: seq,
        },
    ]
}

#[test]
fn every_state_has_its_own_label_in_every_theme() {
    for theme in Theme::ALL {
        let labels: Vec<&str> = every_state().iter().map(|s| theme.state(s)).collect();
        assert_eq!(labels.len(), 9, "the lifecycle is nine variants (ADR-0004)");
        for label in &labels {
            assert!(
                !label.is_empty(),
                "{} left a state unlabelled",
                theme.name()
            );
        }
        let mut distinct = labels.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            labels.len(),
            "{} maps two states onto one label, which hides a transition",
            theme.name()
        );
    }
}

#[test]
fn classic_is_the_identity_map() {
    // The ADR's own cost statement: logs and field names stay military even with
    // the theme off. `classic` shows the variant's own name and does not pretend
    // otherwise.
    for state in every_state() {
        let debug = format!("{state:?}");
        let label = Theme::Classic.state(&state);
        assert!(
            debug.starts_with(label),
            "classic showed {label} for a state whose name is {debug}"
        );
    }
}

#[test]
fn a_theme_relabels_and_never_renames() {
    for state in every_state() {
        let before = format!("{state:?}");
        assert_ne!(
            Theme::Command.state(&state),
            Theme::Classic.state(&state),
            "the two themes agree on {before}, so one of them is not a map"
        );
        assert_eq!(
            before,
            format!("{state:?}"),
            "showing a state changed the state"
        );
    }
}

#[test]
fn the_phases_and_the_mode_are_labelled_in_every_theme() {
    let attempt = [
        AttemptPhase::Localize,
        AttemptPhase::Change,
        AttemptPhase::Measure,
        AttemptPhase::Judge,
        AttemptPhase::Veto,
    ];
    let mission = [
        MissionPhase::Plan,
        MissionPhase::Execute,
        MissionPhase::Integrate,
        MissionPhase::Accept,
    ];
    for theme in Theme::ALL {
        for phase in attempt {
            assert!(!theme.attempt_phase(phase).is_empty());
        }
        for phase in mission {
            assert!(!theme.mission_phase(phase).is_empty());
        }
        for mode in [Mode::SinglePlayer, Mode::CoOp] {
            assert!(!theme.mode(mode).is_empty());
        }
    }
}

#[test]
fn a_cause_is_lineage_and_the_line_says_which_attempt() {
    // Not a theme concern, but the same contract: nothing an operator reads is a
    // `Debug` rendering. Attempts are immutable and every re-run is a fork, so a
    // line about one has to name its parent or the lineage is invisible.
    let parent = AttemptId::at(Seq::new(41));
    for cause in [
        Cause::Retry { of: parent },
        Cause::Rescope { of: parent },
        Cause::Edit { of: parent },
        Cause::Replay { of: parent },
    ] {
        let logged = abcc_core::event::Logged {
            seq: Seq::new(42),
            at_ms: 0,
            event: abcc_core::event::Event::AttemptStarted {
                task: TaskId::at(Seq::new(3)),
                unit: UnitId(0),
                cause,
                checkpoint_from: None,
            },
        };
        let line = abcc_tui::describe(&logged, Theme::Command);
        assert!(
            line.text.contains("a41"),
            "a forked attempt did not name its parent: {}",
            line.text
        );
    }
}
