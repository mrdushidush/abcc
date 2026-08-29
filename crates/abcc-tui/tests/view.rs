//! The fold. Everything the reader shows comes from the log and from nowhere
//! else, and these are the claims that says.

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::{Event, Finish, Logged, Usage};
use abcc_core::outcome::Why;
use abcc_core::run::{AttemptPhase, Mode};
use abcc_core::seq::{AttemptId, MissionId, PromptId, Seq, TaskId, UnitId};
use abcc_core::task::{Command, TaskState};
use abcc_tui::feed::{Feed, FeedError};
use abcc_tui::view::{FEED_LINES, Pulse};
use abcc_tui::{Reader, Replay, Theme, View};

/// The run `abcc-drive` actually produces at Skeleton: one attempt, Localize
/// through a tool call, a checkpoint, and the ending it refuses to call success.
struct Run {
    log: Replay,
    mission: MissionId,
    task: TaskId,
    attempt: AttemptId,
    /// The `ModelCallEnded` — an event that names an attempt and no task.
    model_end: Seq,
}

fn a_skeleton_run() -> Run {
    let mut log = Replay::new();
    let unit = UnitId(0);
    log.push(Event::RunStarted {
        mode: Mode::SinglePlayer,
        version: "0.1.0".to_string(),
        pid: 4242,
    });
    let mission = MissionId::at(log.advance(4).push(Event::MissionCreated {
        title: "Skeleton".to_string(),
    }));
    let task = TaskId::at(log.advance(6).push(Event::TaskCreated {
        mission,
        title: "teach the reader to read".to_string(),
        prompt: "show a run without reading anything but the log".to_string(),
    }));

    let deployed = log.advance(10).next_seq();
    log.push(Event::TaskTransitioned {
        task,
        command: Command::Deploy { unit },
        from: TaskState::Queued,
        to: TaskState::Deployed {
            unit,
            since: deployed,
        },
    });
    let attempt = AttemptId::at(log.advance(120).push(Event::AttemptStarted {
        task,
        unit,
        cause: Cause::Fresh,
        checkpoint_from: None,
    }));
    let engaged = log.advance(2).next_seq();
    log.push(Event::TaskTransitioned {
        task,
        command: Command::Engage { attempt },
        from: TaskState::Deployed {
            unit,
            since: deployed,
        },
        to: TaskState::Engaged {
            attempt,
            since: engaged,
        },
    });

    let model_end = the_attempt_runs(&mut log, task, attempt, engaged);

    Run {
        log,
        mission,
        task,
        attempt,
        model_end,
    }
}

/// The attempt's own events, from Recon through the ending the driver refuses to
/// call success. Split out only because the fixture is long; nothing here is
/// exhaustive over anything, unlike the match it is exercising.
fn the_attempt_runs(log: &mut Replay, task: TaskId, attempt: AttemptId, engaged: Seq) -> Seq {
    log.advance(30).push(Event::AttemptPhaseEntered {
        attempt,
        phase: AttemptPhase::Localize,
    });
    log.advance(5).push(Event::ModelCallStarted {
        attempt,
        provider: "openai-compat".to_string(),
        model: "qwen3.6-35b-a3b-mtp@iq3_s".to_string(),
        head: "Engineering".to_string(),
        budget: 2048,
    });
    let model_end = log.advance(3_200).push(Event::ModelCallEnded {
        attempt,
        usage: Usage {
            prompt_tokens: 495,
            completion_tokens: 180,
            reasoning_tokens: Some(180),
            cached_tokens: None,
        },
        finish: Finish::ToolCalls,
        ttfb_ms: 1_034,
        elapsed_ms: 3_200,
    });
    log.advance(2).push(Event::ToolCallStarted {
        attempt,
        tool: "list_files".to_string(),
        tier: "read".to_string(),
    });
    log.advance(40).push(Event::ToolCallEnded {
        attempt,
        tool: "list_files".to_string(),
        exit: Some(0),
        elapsed_ms: 40,
        unmeasured: None,
        arguments: None,
    });
    log.advance(165).push(Event::CheckpointTaken {
        task,
        sha: "9f2c1ab7c0de4411".to_string(),
        git_ref: "refs/abcc/checkpoints/t3".to_string(),
    });
    log.advance(5).push(Event::AttemptEnded {
        task,
        attempt,
        outcome: AttemptOutcome::Uncertain {
            why: Why::NoCheckerForArtifact {
                artifact: "prose".to_string(),
            },
        },
    });
    let prompt = PromptId::at(log.advance(3).push(Event::OperatorPrompted {
        task,
        attempt,
        question: "the change is at 9f2c1ab7 - is it what you meant?".to_string(),
    }));
    let awaiting = log.advance(1).next_seq();
    log.push(Event::TaskTransitioned {
        task,
        command: Command::RequestOrders { attempt, prompt },
        from: TaskState::Engaged {
            attempt,
            since: engaged,
        },
        to: TaskState::AwaitingOrders {
            attempt,
            prompt,
            since: awaiting,
        },
    });

    model_end
}

fn read(run: &Run) -> Reader {
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&run.log);
    reader
}

#[test]
fn the_board_is_folded_from_the_log_and_nothing_else() {
    let run = a_skeleton_run();
    let reader = read(&run);
    let view = reader.view();

    assert_eq!(view.events(), run.log.len());
    assert_eq!(view.cursor(), run.log.head());
    assert_eq!(view.mode(), Some(Mode::SinglePlayer));
    assert_eq!(view.version(), Some("0.1.0"));
    assert_eq!(view.pid(), Some(4242));
    assert_eq!(view.mission(run.mission), Some("Skeleton"));

    let cards = view.cards();
    assert_eq!(cards.len(), 1);
    let card = cards[0];
    assert_eq!(card.id, run.task);
    assert_eq!(card.title, "teach the reader to read");
    assert_eq!(card.attempts, 1);
    assert!(matches!(card.state, TaskState::AwaitingOrders { .. }));
    // 🚨 The ending the driver refuses to call success reaches the screen as a
    // question with a sha in it, which is the whole point of stopping there.
    assert!(
        card.question
            .as_deref()
            .is_some_and(|q| q.contains("9f2c1ab7")),
        "the operator's question did not reach the card"
    );
}

#[test]
fn a_transition_alone_is_enough_to_draw_a_card() {
    // `TaskTransitioned` carries `from` as well as `to` so that a reader tailing
    // from the middle of the log never has to have seen the earlier events. This
    // is that claim, exercised: one event, no creation, a correct card.
    let task = TaskId::at(Seq::new(3));
    let attempt = AttemptId::at(Seq::new(9));
    let mut view = View::new(Theme::Command);
    view.fold(&Logged {
        seq: Seq::new(88),
        at_ms: 1_000,
        event: Event::TaskTransitioned {
            task,
            command: Command::Accomplish { attempt },
            from: TaskState::Engaged {
                attempt,
                since: Seq::new(40),
            },
            to: TaskState::Accomplished { attempt },
        },
    });

    let card = view.card_at(0).expect("a transition should draw a card");
    assert_eq!(card.id, task);
    assert!(matches!(card.state, TaskState::Accomplished { .. }));
    assert_eq!(card.since, Seq::new(88));
    // And it says so rather than inventing a title it never saw.
    assert!(card.title.is_empty());
    assert!(
        card.label().contains("before this window"),
        "{}",
        card.label()
    );
}

#[test]
fn an_attempts_events_are_its_tasks_heartbeat() {
    // 🚨 v1's F148 defect 2: a task running four model calls inside one span was
    // reaped while perfectly healthy, because the liveness clock did not count
    // the attempt's own events. `ModelCallEnded` names an attempt and no task, so
    // the fold has to resolve it — and if it stops doing so, this fails.
    let run = a_skeleton_run();
    let mut reader = Reader::new(Theme::Command);
    reader.rewind(&run.log, run.model_end);

    let card = reader.card().expect("a card");
    assert_eq!(
        card.last_seq, run.model_end,
        "an attempt-only event did not move the task's progress clock"
    );
    assert_eq!(
        run.log
            .read_from(Seq::ORIGIN, 64)
            .unwrap()
            .iter()
            .find(|l| l.seq == run.model_end)
            .and_then(|l| l.event.task()),
        None,
        "this test is worthless unless that event names no task"
    );
    assert_eq!(
        reader.view().cards()[0].attempts,
        1,
        "the mapping comes from AttemptStarted, which is also the attempt count"
    );
    assert_eq!(run.attempt.born(), Seq::new(5));
}

#[test]
fn the_feed_is_bounded_and_keeps_the_tail() {
    // F112: the donor's frontend OOM is what its ring buffer was introduced to
    // fix, and this log is *designed* to grow — it is the replay source too.
    let mut log = Replay::new();
    for i in 0..FEED_LINES + 20 {
        log.advance(1).push(Event::Note {
            text: format!("note {i}"),
        });
    }
    let mut reader = Reader::new(Theme::Command);
    while reader.pump(&log) > 0 {}

    let feed = reader.view().feed();
    assert_eq!(feed.len(), FEED_LINES);
    assert_eq!(feed.front().unwrap().text, "note 20");
    assert_eq!(
        feed.back().unwrap().text,
        format!("note {}", FEED_LINES + 19)
    );
    assert_eq!(reader.view().events(), FEED_LINES + 20, "counted, not kept");
}

#[test]
fn replay_is_the_live_view_at_a_different_position() {
    // ADR-0012: the after-action screen is nearly free because it is the same
    // paged read positioned at a different `seq`. If that is true, a reader that
    // rewound to the head is indistinguishable from one that tailed there.
    let run = a_skeleton_run();
    let tailed = read(&run);

    let mut rewound = read(&run);
    rewound.rewind(&run.log, run.log.head());
    assert_eq!(tailed.view(), rewound.view());
    assert!(!rewound.following(), "rewinding leaves the tail");

    // And a position in the middle is a fold of exactly that prefix.
    let mut middle = read(&run);
    middle.rewind(&run.log, run.model_end);
    let mut prefix = View::new(Theme::Command);
    let through = usize::try_from(run.model_end.get()).unwrap();
    prefix.fold_all(&run.log.read_from(Seq::ORIGIN, through).unwrap());
    assert_eq!(middle.view(), &prefix);
    assert_eq!(middle.view().cursor(), run.model_end);
}

#[test]
fn the_pulse_is_the_clock_the_state_contract_names() {
    let run = a_skeleton_run();

    // Engaged: the progress clock, and the ten-second bar is the only warning
    // this reader ever raises.
    let mut engaged = Reader::new(Theme::Command);
    engaged.rewind(&run.log, Seq::new(run.log.head().get() - 1));
    let card = engaged.card().unwrap();
    assert!(matches!(card.state, TaskState::Engaged { .. }));
    let quiet = engaged.view().pulse(card, card.last_at_ms + 9_000);
    assert!(
        !quiet.stalled(),
        "nine seconds is inside the bar: {quiet:?}"
    );
    let stalled = engaged.view().pulse(card, card.last_at_ms + 11_000);
    assert!(stalled.stalled(), "eleven seconds is over it: {stalled:?}");

    // AwaitingOrders: a human clock. Measured, displayed, and never a warning —
    // the watchdog's whole job is telling *no human yet* from *no progress*.
    let awaiting = read(&run);
    let card = awaiting.card().unwrap();
    let after_an_hour = awaiting.view().pulse(card, card.since_at_ms + 3_600_000);
    assert_eq!(after_an_hour, Pulse::Waiting { ms: 3_600_000 });
    assert!(!after_an_hour.stalled());

    // Queued: nothing is running and nothing is waiting.
    let mut fresh = View::new(Theme::Command);
    fresh.fold(&Logged {
        seq: Seq::new(1),
        at_ms: 0,
        event: Event::TaskCreated {
            mission: MissionId::at(Seq::new(1)),
            title: "not started".to_string(),
            prompt: String::new(),
        },
    });
    let card = fresh.card_at(0).unwrap();
    assert_eq!(fresh.pulse(card, 10_000_000), Pulse::Untimed);
}

#[test]
fn review_minutes_are_summed_from_the_log() {
    // W13's ladder is measured in human review minutes per merged change, and
    // starting the measurement later leaves it with no baseline. Nothing writes
    // this event yet; an empty ladder reads as zero of zero, which is honest.
    let empty = View::new(Theme::Command);
    assert_eq!(empty.review(), (0, 0));

    let mut log = Replay::new();
    log.push(Event::ReviewRecorded {
        change: "d2ed3bb".to_string(),
        seconds: 750,
        by: "david".to_string(),
        crossed_boundary: true,
    });
    log.advance(1).push(Event::ReviewRecorded {
        change: "3b28740".to_string(),
        seconds: 90,
        by: "david".to_string(),
        crossed_boundary: false,
    });
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    assert_eq!(reader.view().review(), (840, 2));
    assert!(
        reader.view().feed()[0]
            .text
            .contains("crosses a module boundary"),
        "M3 counts boundary-crossing changes, so the line has to say which"
    );
}

#[test]
fn a_log_that_cannot_be_read_is_shown_and_never_swallowed() {
    struct Broken;
    impl Feed for Broken {
        fn read_from(&self, since: Seq, _limit: usize) -> Result<Vec<Logged>, FeedError> {
            Err(FeedError::new(since, "database is locked"))
        }
    }

    let mut reader = Reader::new(Theme::Command);
    assert_eq!(reader.pump(&Broken), 0);
    let error = reader.error().expect("the failure has to reach the screen");
    assert!(error.contains("database is locked"), "{error}");

    // And it clears when the log answers again, rather than sticking around as a
    // scare after the thing it described has gone.
    let run = a_skeleton_run();
    reader.pump(&run.log);
    assert!(reader.error().is_none());
}
