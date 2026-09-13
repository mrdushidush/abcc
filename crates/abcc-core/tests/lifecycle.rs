//! The lifecycle's invariants, each written as the v1 defect it forecloses.
//!
//! W3's four liveness holes (F148) are one missing concept — a per-state
//! contract — and these tests are that contract asserted rather than described.

use abcc_core::attempt::Cause;
use abcc_core::run::{AttemptPhase, Mode};
use abcc_core::seq::{AttemptId, CheckpointId, PromptId, Seq, UnitId};
use abcc_core::task::{
    AbortReason, BootAction, Command, Liveness, Refused, RequeueReason, TaskState, Watchdog,
};

fn seq(n: i64) -> Seq {
    Seq::new(n)
}

fn attempt(n: i64) -> AttemptId {
    AttemptId::at(seq(n))
}

/// One of every variant. If a tenth is added, this list is where it is felt
/// first — every invariant below runs over it.
fn every_state() -> Vec<TaskState> {
    vec![
        TaskState::Queued,
        TaskState::Deployed {
            unit: UnitId(0),
            since: seq(10),
        },
        TaskState::Engaged {
            attempt: attempt(11),
            since: seq(11),
        },
        TaskState::AwaitingOrders {
            attempt: attempt(11),
            prompt: PromptId::at(seq(12)),
            since: seq(12),
        },
        TaskState::Holding {
            checkpoint: CheckpointId::at(seq(13)),
            since: seq(13),
        },
        TaskState::Commandeered {
            operator_since: seq(14),
        },
        TaskState::Accomplished {
            attempt: attempt(11),
        },
        TaskState::Failed {
            attempt: attempt(11),
        },
        TaskState::Aborted {
            reason: AbortReason::Operator { by: "david".into() },
            since: seq(15),
        },
    ]
}

#[test]
fn there_are_nine_states_and_three_of_them_are_terminal() {
    let states = every_state();
    assert_eq!(states.len(), 9);
    assert_eq!(states.iter().filter(|s| s.is_terminal()).count(), 3);
}

/// ADR-0004's invariant, and the one the writer enforces: **terminal implies
/// zero resources.** v1's recovery path is a complete eight-step cleanup that
/// leaves the task not re-queued, which is the same bug seen from the other end.
#[test]
fn terminal_states_hold_nothing() {
    for s in every_state() {
        let c = s.contract();
        if c.terminal {
            assert!(!c.holds_slot, "{} holds a slot", s.name());
            assert!(!c.holds_workspace, "{} holds a workspace", s.name());
            assert_eq!(c.liveness, Liveness::None, "{}", s.name());
            assert_eq!(c.watchdog, Watchdog::NotWatched, "{}", s.name());
        }
    }
}

/// 🚨 Operator states are watched but never reaped. The watchdog distinguishes
/// *no human yet* from *no progress* — the distinction F91's forty silent
/// minutes taught the harness.
#[test]
fn operator_states_are_watched_and_never_reaped() {
    for s in every_state() {
        let c = s.contract();
        if c.liveness == Liveness::Human {
            assert_eq!(
                c.watchdog,
                Watchdog::WatchNeverReap,
                "{} has a human clock and a reaper",
                s.name()
            );
        }
        if matches!(c.watchdog, Watchdog::Reap { .. }) {
            assert_ne!(
                c.liveness,
                Liveness::Human,
                "{} would reap a human",
                s.name()
            );
            assert_ne!(
                c.liveness,
                Liveness::None,
                "{} would reap on a clock it does not have",
                s.name()
            );
        }
    }
}

/// A reapable state must hold something worth reclaiming, and anything holding a
/// slot must either be reaped or be waiting on a human who can see it. A state
/// that holds the fleet's scarcest resource and is watched by nobody is exactly
/// how v1 lost `assigned`: written by five sites and advanced by none.
#[test]
fn nothing_holds_a_slot_unwatched() {
    for s in every_state() {
        let c = s.contract();
        if c.holds_slot {
            assert_ne!(
                c.watchdog,
                Watchdog::NotWatched,
                "{} holds a slot and nothing is watching it",
                s.name()
            );
        }
        if matches!(c.watchdog, Watchdog::Reap { .. }) {
            assert!(
                c.holds_slot || c.holds_workspace,
                "{} is reaped and holds nothing",
                s.name()
            );
        }
    }
}

/// The two states whose boot action moves the task are exactly the two that
/// name an in-process resource. `AwaitingOrders` re-presents rather than
/// answering, and everything else stands.
#[test]
fn boot_moves_exactly_the_states_that_named_a_dead_resource() {
    for s in every_state() {
        let c = s.contract();
        match c.boot {
            BootAction::Requeue => assert!(
                c.holds_slot,
                "{} is requeued at boot but held no slot",
                s.name()
            ),
            BootAction::RepresentPrompt => assert_eq!(
                c.liveness,
                Liveness::Human,
                "{} re-presents a prompt without a human clock",
                s.name()
            ),
            BootAction::Stands => assert!(
                !c.holds_slot,
                "{} stands at boot while holding a slot that did not survive",
                s.name()
            ),
        }
    }
}

/// W3's contract table, row `AwaitingOrders`: the slot is *released at the
/// boundary* and the workspace is kept. A human clock has no timeout, so a slot
/// held by a sleeping operator is half the fleet (N=2, ADR-0003).
#[test]
fn awaiting_orders_releases_the_slot_and_keeps_the_workspace() {
    let s = TaskState::AwaitingOrders {
        attempt: attempt(11),
        prompt: PromptId::at(seq(12)),
        since: seq(12),
    };
    let c = s.contract();
    assert!(!c.holds_slot);
    assert!(c.holds_workspace);
    assert_eq!(c.boot, BootAction::RepresentPrompt);
}

/// `Engaged`'s clock is progress — the seq of the attempt's last event — so a
/// task running four model calls inside one span is not reaped while healthy.
/// v1 reads `assignedAt`, written once at assignment and refreshed never, and
/// reaps at five minutes.
#[test]
fn engaged_is_watched_on_progress_and_not_on_time_since_assignment() {
    let s = TaskState::Engaged {
        attempt: attempt(11),
        since: seq(11),
    };
    assert_eq!(s.contract().liveness, Liveness::Progress);
}

#[test]
fn no_command_resurrects_a_terminal_task() {
    let commands = [
        Command::Deploy { unit: UnitId(0) },
        Command::Engage {
            attempt: attempt(20),
        },
        Command::Commandeer,
        Command::Release,
        Command::Requeue {
            why: RequeueReason::OrphanedByRestart,
        },
        Command::Abort {
            reason: AbortReason::Operator { by: "x".into() },
        },
    ];
    for s in every_state().into_iter().filter(TaskState::is_terminal) {
        for c in &commands {
            let err = s
                .apply(c, seq(99))
                .expect_err("a terminal task accepted a command");
            assert!(
                matches!(err, Refused::Terminal { .. }),
                "{} accepted {} with {err}",
                s.name(),
                c.name()
            );
        }
    }
}

/// The operator's two escape hatches work from anywhere the fleet can be, and
/// they do not depend on where it happened to be.
#[test]
fn abort_and_commandeer_are_legal_from_every_non_terminal_state() {
    for s in every_state().into_iter().filter(|s| !s.is_terminal()) {
        let aborted = s
            .apply(
                &Command::Abort {
                    reason: AbortReason::Operator { by: "david".into() },
                },
                seq(50),
            )
            .unwrap_or_else(|e| panic!("{} refused Abort: {e}", s.name()));
        assert!(matches!(aborted, TaskState::Aborted { since, .. } if since == seq(50)));

        let taken = s
            .apply(&Command::Commandeer, seq(51))
            .unwrap_or_else(|e| panic!("{} refused Commandeer: {e}", s.name()));
        assert_eq!(
            taken,
            TaskState::Commandeered {
                operator_since: seq(51)
            }
        );
    }
}

/// v1 has four retry mechanisms and all of them mutate the row they are handed,
/// so a stale reply ends whatever is running now. Here a command that names an
/// attempt must name the one in flight.
#[test]
fn a_command_naming_a_stale_attempt_is_refused() {
    let s = TaskState::Engaged {
        attempt: attempt(11),
        since: seq(11),
    };
    let err = s
        .apply(
            &Command::Accomplish {
                attempt: attempt(7),
            },
            seq(30),
        )
        .expect_err("stale attempt accepted");
    assert!(matches!(err, Refused::WrongAttempt { .. }), "{err}");

    // And the same command naming the live one is fine.
    let ok = s
        .apply(
            &Command::Accomplish {
                attempt: attempt(11),
            },
            seq(30),
        )
        .expect("live attempt refused");
    assert_eq!(
        ok,
        TaskState::Accomplished {
            attempt: attempt(11)
        }
    );
}

/// Every state's `since` is the seq of the transition that produced it, which is
/// what makes *when* and *where in the replay* the same fact.
#[test]
fn since_is_the_seq_of_the_transition_that_produced_it() {
    let at = seq(4242);
    let deployed = TaskState::Queued
        .apply(&Command::Deploy { unit: UnitId(1) }, at)
        .expect("deploy");
    assert!(matches!(deployed, TaskState::Deployed { since, .. } if since == at));

    let engaged = deployed
        .apply(
            &Command::Engage {
                attempt: attempt(4243),
            },
            seq(4243),
        )
        .expect("engage");
    assert!(matches!(engaged, TaskState::Engaged { since, .. } if since == seq(4243)));
}

/// The happy path, end to end, as the Skeleton milestone runs it.
#[test]
fn one_task_runs_from_queued_to_accomplished() {
    let a = attempt(101);
    let s = TaskState::Queued;
    let s = s
        .apply(&Command::Deploy { unit: UnitId(0) }, seq(100))
        .unwrap();
    let s = s.apply(&Command::Engage { attempt: a }, seq(101)).unwrap();
    let s = s
        .apply(&Command::Accomplish { attempt: a }, seq(150))
        .unwrap();
    assert_eq!(s, TaskState::Accomplished { attempt: a });
    assert!(s.is_terminal());
    assert!(!s.contract().holds_slot);
}

/// The intervention path, the slot handed back on the way out — and
/// 🚨 **nothing that puts the task straight back on one.**
///
/// This test used to end by applying `Command::OrdersGiven` and asserting
/// `Deployed`, and **it was that command's only caller anywhere** (F703), which
/// is how an edge nothing drives survived a passing suite for a whole phase.
/// The command is gone (F732); what is asserted instead is the route a refused
/// task really takes back onto the board, which is the one
/// `abcc-drive/tests/attempt.rs` flies: the operator takes the keyboard and
/// hands it back, and the task re-enters through `Queued` like everything else.
#[test]
fn an_attempt_that_asks_for_orders_comes_back_through_the_operator() {
    let a = attempt(201);
    let p = PromptId::at(seq(210));
    let s = TaskState::Queued
        .apply(&Command::Deploy { unit: UnitId(0) }, seq(200))
        .and_then(|s| s.apply(&Command::Engage { attempt: a }, seq(201)))
        .and_then(|s| {
            s.apply(
                &Command::RequestOrders {
                    attempt: a,
                    prompt: p,
                },
                seq(210),
            )
        })
        .expect("request orders");
    assert!(!s.contract().holds_slot, "the slot was not released");

    // No command reaches a slot from here. `Engage` names the attempt that is
    // still in flight, so it gets past the wrong-attempt guard and is refused
    // by the table itself rather than by the guard — which is the assertion
    // worth having.
    for command in [
        Command::Deploy { unit: UnitId(1) },
        Command::Engage { attempt: a },
        Command::Resume { unit: UnitId(1) },
        Command::Requeue {
            why: RequeueReason::AttemptRetryable { of: a },
        },
    ] {
        assert!(
            matches!(
                s.apply(&command, seq(300)),
                Err(Refused::NotLegalHere { .. })
            ),
            "{} put an intervention-required task back on a slot",
            command.name()
        );
    }

    // The route that exists, and the one the archive has actually taken.
    let taken = s.apply(&Command::Commandeer, seq(300)).expect("commandeer");
    assert_eq!(
        taken,
        TaskState::Commandeered {
            operator_since: seq(300)
        }
    );
    let back = taken.apply(&Command::Release, seq(301)).expect("release");
    assert_eq!(back, TaskState::Queued, "release returns it to the board");
}

/// Stop-but-keep-the-work is a transition to a state, and coming back off hold
/// re-acquires a slot explicitly.
#[test]
fn holding_keeps_the_workspace_and_gives_back_the_slot() {
    let a = attempt(401);
    let cp = CheckpointId::at(seq(410));
    let held = TaskState::Queued
        .apply(&Command::Deploy { unit: UnitId(0) }, seq(400))
        .and_then(|s| s.apply(&Command::Engage { attempt: a }, seq(401)))
        .and_then(|s| s.apply(&Command::Hold { checkpoint: cp }, seq(410)))
        .expect("hold");
    let c = held.contract();
    assert!(!c.holds_slot);
    assert!(c.holds_workspace);
    assert_eq!(c.boot, BootAction::Stands);

    let back = held
        .apply(&Command::Resume { unit: UnitId(0) }, seq(500))
        .expect("resume");
    assert!(matches!(back, TaskState::Deployed { .. }));
}

/// Every state crosses the log as JSON and comes back the same. Boot is a replay
/// of these bytes, so a state that cannot be read back is a state that survives
/// exactly one process.
#[test]
fn every_state_round_trips_through_json() {
    for s in every_state() {
        let json = serde_json::to_string(&s).expect("serialize");
        let back: TaskState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back, "{json}");
    }
}

/// 🚨 David's ruling of 2026-08-28, held strictly: **only deterministic rungs may
/// refuse.** The Judge reports, and its report is never a vote. There is no
/// false-fail rate to tune, because a false fail is a bug in a rung.
#[test]
fn only_the_phases_without_a_model_may_refuse() {
    for phase in [
        AttemptPhase::Localize,
        AttemptPhase::Change,
        AttemptPhase::Measure,
        AttemptPhase::Judge,
        AttemptPhase::Veto,
    ] {
        assert_eq!(
            phase.may_refuse(),
            !phase.uses_model(),
            "{phase:?} disagrees with the ruling"
        );
    }
    assert!(!AttemptPhase::Judge.may_refuse());
    assert!(AttemptPhase::Measure.may_refuse());
    assert!(AttemptPhase::Veto.may_refuse());
}

/// Mode is a one-way sticky ratchet. An upgrade mid-run would make the run's own
/// egress record retroactively wrong.
#[test]
fn mode_only_ratchets_downward() {
    assert!(Mode::CoOp.may_move_to(Mode::SinglePlayer));
    assert!(!Mode::SinglePlayer.may_move_to(Mode::CoOp));
    assert!(!Mode::CoOp.may_move_to(Mode::CoOp));
    assert!(!Mode::SinglePlayer.may_move_to(Mode::SinglePlayer));
}

/// A rescope or an edit is the operator changing the question, so it does not
/// spend the retry budget. v1 folded all four causes into one counter and then
/// could not explain the number.
#[test]
fn only_a_retry_spends_the_retry_budget() {
    let of = attempt(1);
    assert!(!Cause::Fresh.spends_retry_budget());
    assert!(Cause::Retry { of }.spends_retry_budget());
    assert!(!Cause::Rescope { of }.spends_retry_budget());
    assert!(!Cause::Edit { of }.spends_retry_budget());
    assert!(!Cause::Replay { of }.spends_retry_budget());

    assert_eq!(Cause::Fresh.parent(), None);
    assert_eq!(Cause::Retry { of }.parent(), Some(of));
}
