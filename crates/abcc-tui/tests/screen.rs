//! What is actually on the screen, drawn into a buffer rather than a terminal.
//!
//! These are the tests that make the reader more than a fold: the Skeleton exit
//! criterion is *"the reader shows that run"*, and a projection nobody drew shows
//! nothing.

use abcc_core::attempt::Cause;
use abcc_core::event::Event;
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, MissionId, PromptId, Seq, TaskId, UnitId};
use abcc_core::task::{Command, TaskState};
use abcc_tui::{Reader, Replay, Theme, draw};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Four events: a run, a task, an attempt, and the transition that engages it.
fn engaged() -> (Replay, TaskId) {
    let mut log = Replay::new();
    let unit = UnitId(0);
    log.push(Event::RunStarted {
        mode: Mode::SinglePlayer,
        version: "0.1.0".to_string(),
        pid: 4242,
    });
    let task = TaskId::at(log.advance(6).push(Event::TaskCreated {
        mission: MissionId::at(Seq::new(1)),
        title: "teach the reader to read".to_string(),
        prompt: "show a run".to_string(),
    }));
    let attempt = AttemptId::at(log.advance(30).push(Event::AttemptStarted {
        task,
        unit,
        cause: Cause::Fresh,
        checkpoint_from: None,
    }));
    let since = log.advance(2).next_seq();
    log.push(Event::TaskTransitioned {
        task,
        command: Command::Engage { attempt },
        from: TaskState::Deployed {
            unit,
            since: Seq::new(2),
        },
        to: TaskState::Engaged { attempt, since },
    });
    (log, task)
}

fn rows(reader: &Reader, now_ms: i64, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, reader, now_ms)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

fn shows(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|r| r.contains(needle))
}

#[test]
fn the_reader_draws_the_run_from_the_log_alone() {
    let (log, _) = engaged();
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    let screen = rows(&reader, 40, 110, 26);

    // The header: the run's mode is its first event, and the cursor is a `seq`.
    assert!(shows(&screen, "SINGLE PLAYER"), "{screen:#?}");
    assert!(shows(&screen, "v0.1.0"));
    assert!(shows(&screen, "pid 4242"));
    assert!(shows(&screen, "seq 4"));
    assert!(shows(&screen, "LIVE"));
    // The board, under the labels the states were named against.
    assert!(shows(&screen, "ENGAGING TARGET"));
    assert!(shows(&screen, "teach the reader to read"));
    // The feed, one event per line, in `seq` order.
    assert!(shows(&screen, "run started"));
    assert!(shows(&screen, "attempt 3 started on unit-0"));
    // The footer: the contract, which is what every control verb ends up asking.
    assert!(shows(&screen, "holds a slot"));
    assert!(shows(&screen, "requeued by a restart"));
    // And the ladder, empty and saying so rather than absent.
    assert!(shows(&screen, "review 0.0 min / 0"));
}

#[test]
fn a_stalled_attempt_is_marked_and_a_waiting_human_is_not() {
    // 🚨 W5's ten-second bar, on the screen. The harness once sat silent for
    // forty minutes and could not tell that from a hang — and the distinction
    // that matters is *no progress* against *no human yet*, which is why only one
    // of these two is a warning.
    let (log, task) = engaged();
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    let last = reader.card().unwrap().last_at_ms;

    let quiet = rows(&reader, last + 9_000, 110, 26);
    assert!(!shows(&quiet, "SILENT"), "nine seconds is inside the bar");
    let stalled = rows(&reader, last + 30_000, 110, 26);
    assert!(shows(&stalled, "SILENT for 30s"), "{stalled:#?}");
    assert!(
        stalled.iter().any(|r| r.contains("!t2")),
        "the board did not mark the stalled task"
    );

    // The same task, waiting on a human instead. No mark, however long it takes.
    let mut log = log;
    let attempt = AttemptId::at(Seq::new(3));
    let prompt = PromptId::at(Seq::new(5));
    log.advance(10).push(Event::OperatorPrompted {
        task,
        attempt,
        question: "does 9f2c1ab7 do what you meant?".to_string(),
    });
    let since = log.advance(1).next_seq();
    log.push(Event::TaskTransitioned {
        task,
        command: Command::RequestOrders { attempt, prompt },
        from: TaskState::Engaged {
            attempt,
            since: Seq::new(4),
        },
        to: TaskState::AwaitingOrders {
            attempt,
            prompt,
            since,
        },
    });
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    let waited = rows(
        &reader,
        reader.card().unwrap().since_at_ms + 3_600_000,
        110,
        26,
    );
    assert!(shows(&waited, "INTERVENTION REQUIRED"));
    assert!(
        shows(&waited, "waiting on a human for 60m00s"),
        "{waited:#?}"
    );
    assert!(
        !shows(&waited, "SILENT"),
        "a human clock is never a warning"
    );
    assert!(shows(&waited, "does 9f2c1ab7 do what you meant?"));
}

#[test]
fn an_empty_log_says_that_rather_than_drawing_a_healthy_board() {
    let reader = Reader::new(Theme::Command);
    let screen = rows(&reader, 0, 110, 26);
    assert!(shows(&screen, "no tasks on the log yet"), "{screen:#?}");
    assert!(shows(&screen, "nothing on the log at this position"));
    assert!(shows(&screen, "mode not yet seen"));
    assert!(shows(&screen, "nothing selected"));
}

#[test]
fn the_theme_key_relabels_the_whole_screen_by_replaying_it() {
    // A feed line is text by the time the reader holds it, so changing the label
    // map re-folds. That costs one replay of what is on screen and it exercises
    // the path the after-action view uses.
    let (log, _) = engaged();
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    assert!(shows(&rows(&reader, 40, 110, 26), "ENGAGING TARGET"));

    let intent = reader.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
    reader.act(&log, intent);
    let screen = rows(&reader, 40, 110, 26);
    assert!(shows(&screen, "Engaged"), "{screen:#?}");
    assert!(!shows(&screen, "ENGAGING TARGET"));
    assert!(shows(&screen, "SinglePlayer"));
    assert_eq!(
        reader.view().events(),
        4,
        "the replay reproduced the same position"
    );
}

#[test]
fn it_draws_at_the_sizes_a_real_terminal_comes_in() {
    let (log, _) = engaged();
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    for (w, h) in [(40, 10), (80, 24), (120, 40), (240, 60)] {
        let screen = rows(&reader, 40, w, h);
        assert_eq!(screen.len(), usize::from(h));
        assert!(
            screen.iter().all(|r| r.chars().count() == usize::from(w)),
            "{w}x{h} drew a ragged buffer"
        );
    }
}
