//! The durable [`Feed`]: the reader's paged read, against the real `SQLite` log.
//!
//! 🚨 `abcc-tui` proves its fold against `Replay`, an in-memory double, and it is
//! written that way on purpose — putting rusqlite behind the trait would make the
//! seam decorative. The consequence is that **the adapter is the one piece of the
//! read path that the reader's own tests cannot reach**, so it is proved here,
//! against a database on disk, with the same claims: a board comes out of the log
//! alone, the cursor advances, and a rewind is the live view at a different
//! position.

use std::path::Path;

use abcc::feed::StoreFeed;
use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::{Composition, Event, Finish, Usage};
use abcc_core::outcome::Why;
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, MissionId, Seq, TaskId, UnitId};
use abcc_core::task::{Command, TaskState};
use abcc_store::Store;
use abcc_tui::feed::Feed;
use abcc_tui::reader::{Intent, PAGE, Reader};
use abcc_tui::{Theme, View};

/// A log with a run on it: a mission, a task, an attempt that made a model call,
/// and an ending. Written through the real store, in a real file.
fn seeded(path: &Path) -> TaskId {
    let mut store = Store::open(path).expect("open");
    store
        .append(Event::RunStarted {
            mode: Mode::SinglePlayer,
            version: "0.1.0".into(),
            pid: 4242,
        })
        .expect("run");
    let mission = store
        .append(Event::MissionCreated {
            title: "skeleton".into(),
        })
        .expect("mission");
    let created = store
        .append(Event::TaskCreated {
            mission: MissionId::at(mission.seq),
            title: "one returns two".into(),
            prompt: "make one() return two".into(),
        })
        .expect("task");
    let task = TaskId::at(created.seq);

    store
        .apply(task, Command::Deploy { unit: UnitId(0) })
        .expect("deploy");
    let started = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: Cause::Fresh,
            checkpoint_from: None,
        })
        .expect("attempt");
    let attempt = AttemptId::at(started.seq);
    store
        .apply(task, Command::Engage { attempt })
        .expect("engage");
    // ⚠ Names an attempt and no task. The fold has to resolve it, or a task
    // running several model calls in one span reads as stalled — v1's F148
    // defect 2, and the reason this event is in the fixture at all.
    store
        .append(Event::ModelCallEnded {
            attempt,
            usage: Usage {
                prompt_tokens: 495,
                completion_tokens: 180,
                reasoning_tokens: Some(180),
                cached_tokens: None,
            },
            finish: Finish::Stop,
            ttfb_ms: 146,
            elapsed_ms: 2_400,
            composition: Some(Composition {
                text_chars: 41,
                reasoning_chars: 700,
                calls: Vec::new(),
            }),
        })
        .expect("call");
    store
        .append(Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Uncertain {
                why: Why::NoCheckerForArtifact {
                    artifact: "aa8b8c0".into(),
                },
            },
        })
        .expect("ended");
    store.apply(task, Command::Fail { attempt }).expect("fail");
    task
}

fn read(path: &Path) -> StoreFeed {
    StoreFeed::open(path).expect("feed")
}

fn folded(feed: &StoreFeed) -> Reader {
    let mut reader = Reader::new(Theme::Command);
    // Pump until a page comes back empty, the way the binary's loop does.
    while reader.pump(feed) > 0 {}
    reader
}

// ---------------------------------------------------------------------------
// a board, out of the log alone
// ---------------------------------------------------------------------------

#[test]
fn a_board_comes_out_of_the_log_and_nothing_else() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let feed = read(&log);
    let reader = folded(&feed);
    let view = reader.view();

    // The run header, which only `RunStarted` can supply.
    assert_eq!(view.mode(), Some(Mode::SinglePlayer));
    assert_eq!(view.version(), Some("0.1.0"));
    assert_eq!(view.pid(), Some(4242));

    let cards = view.cards();
    assert_eq!(cards.len(), 1);
    let card = cards[0];
    assert_eq!(card.id, task);
    assert_eq!(card.title, "one returns two");
    assert!(matches!(card.state, TaskState::Failed { .. }));
    assert_eq!(card.attempts, 1);
    assert_eq!(
        view.mission(card.mission.expect("a mission")),
        Some("skeleton")
    );
}

#[test]
fn an_attempts_events_are_its_tasks_heartbeat() {
    // `ModelCallEnded` names an attempt and no task, so the fold has to have
    // learned the mapping at `AttemptStarted`. Without that a task running four
    // model calls inside one span reads as stalled.
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    seeded(&log);

    let feed = read(&log);
    let reader = folded(&feed);
    let card = reader.view().cards()[0];
    let call = feed
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find(|l| matches!(l.event, Event::ModelCallEnded { .. }))
        .expect("a model call");
    assert!(
        card.last_seq >= call.seq,
        "the model call at {} did not reach the card's clock ({})",
        call.seq,
        card.last_seq
    );
}

// ---------------------------------------------------------------------------
// the cursor
// ---------------------------------------------------------------------------

#[test]
fn a_page_is_bounded_and_the_cursor_advances_past_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let mut store = Store::open(&log).expect("open");
    for i in 0..(PAGE + 20) {
        store
            .append(Event::Note {
                text: format!("note {i}"),
            })
            .expect("note");
    }
    drop(store);

    let feed = read(&log);
    let page = feed.read_from(Seq::ORIGIN, PAGE).expect("page");
    assert_eq!(
        page.len(),
        PAGE,
        "a page is bounded by the limit it asked for"
    );

    let mut reader = Reader::new(Theme::Command);
    assert_eq!(reader.pump(&feed), PAGE);
    assert_eq!(reader.pump(&feed), 20);
    // An empty page means *nothing new yet*, never *the end*.
    assert_eq!(reader.pump(&feed), 0);
    assert_eq!(reader.view().events(), PAGE + 20);
    assert!(reader.error().is_none());
}

#[test]
fn the_head_is_the_last_seq_written() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    seeded(&log);
    let feed = read(&log);
    let last = feed
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .last()
        .expect("events")
        .seq;
    assert_eq!(feed.head().expect("head"), last);
}

// ---------------------------------------------------------------------------
// replay is the live view at a different position
// ---------------------------------------------------------------------------

#[test]
fn rewinding_the_durable_feed_lands_where_the_tail_was() {
    // The same claim `abcc-tui` makes against its in-memory double, now against
    // rusqlite: the rewind refolds from the origin through the same paged read
    // the tail uses, so a reader in the past cannot drift from one at the head.
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    seeded(&log);

    let feed = read(&log);
    let live = folded(&feed);
    let at = live.view().cursor();

    let mut rewound = Reader::new(Theme::Command);
    rewound.rewind(&feed, at);

    assert_eq!(rewound.view(), live.view());
    assert!(!rewound.following(), "a rewind stops following");
}

#[test]
fn a_position_in_the_middle_shows_the_board_as_it_was_there() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let feed = read(&log);
    // The seq at which the task was engaged: before the ending, after the start.
    let engaged = feed
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find(|l| {
            matches!(&l.event, Event::TaskTransitioned { to, .. } if matches!(to, TaskState::Engaged { .. }))
        })
        .expect("an engagement")
        .seq;

    let mut reader = Reader::new(Theme::Command);
    reader.rewind(&feed, engaged);
    let card = reader.view().cards()[0];
    assert_eq!(card.id, task);
    assert!(
        matches!(card.state, TaskState::Engaged { .. }),
        "the board at {engaged} should be mid-attempt, and was {:?}",
        card.state
    );

    // And stepping forward from there reaches the ending, through the same feed.
    let intent = reader.act(&feed, Intent::Step);
    assert_eq!(intent, Intent::Step);
    while reader.pump(&feed) > 0 {}
    assert!(matches!(
        reader.view().cards()[0].state,
        TaskState::Failed { .. }
    ));
}

// ---------------------------------------------------------------------------
// a read that did not happen
// ---------------------------------------------------------------------------

#[test]
fn a_log_that_is_not_one_fails_at_open_rather_than_drawing_an_empty_board() {
    // 🚨 An empty board and a board that could not be read look identical, and
    // the second is the failure this whole design is arranged against. The
    // adapter refuses at `open` rather than answering every page with nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let not_a_log = dir.path().join("log.sqlite");
    std::fs::write(&not_a_log, b"this is not a database").expect("write");
    assert!(StoreFeed::open(&not_a_log).is_err());
}

#[test]
fn an_empty_log_is_an_empty_board_and_says_nothing_else() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    drop(Store::open(&log).expect("open"));

    let feed = read(&log);
    let reader = folded(&feed);
    assert_eq!(reader.view().cards().len(), 0);
    assert_eq!(reader.view().events(), 0);
    // Not "unknown mode": nothing has said what the mode is, which is different.
    assert_eq!(reader.view().mode(), None);
    assert_eq!(reader.view(), &View::new(Theme::Command));
}

// ---------------------------------------------------------------------------
// the instrument
// ---------------------------------------------------------------------------

/// Fold a **real** log — one an actual run wrote — and print the board.
///
/// `#[ignore]`d and pointed by `ABCC_LOG`, the same shape as `abcc-store`'s
/// `durability_rate` and `abcc-vcs`'s `cycle_cost`: an instrument that is kept
/// because the claim it checks is about a log this workspace did not synthesise.
///
/// ```text
/// ABCC_LOG=%LOCALAPPDATA%/abcc/<repo>/log.sqlite \
///   cargo test -p abcc --test feed -- --ignored --nocapture
/// ```
///
/// 🚨 It opens the log through [`StoreFeed`] and reads `event` alone. It cannot
/// consult the `task` projection, because nothing in scope can — which is the
/// point: whatever it prints came out of the log.
#[test]
#[ignore = "needs a real log; pass ABCC_LOG"]
fn the_reader_shows_a_real_run_from_the_log_alone() {
    let Ok(path) = std::env::var("ABCC_LOG") else {
        panic!("set ABCC_LOG to a log an actual run wrote");
    };
    let feed = StoreFeed::open(Path::new(&path)).expect("open the real log");
    let reader = folded(&feed);
    let view = reader.view();

    println!(
        "{} events, mode {:?}, version {:?}, pid {:?}",
        view.events(),
        view.mode(),
        view.version(),
        view.pid()
    );
    let (minutes, changes) = view.review();
    println!("review {minutes} min over {changes} change(s)");
    for card in view.cards() {
        println!(
            "  {:<6} {:<22} attempts {} last seq {}  {}",
            card.id.to_string(),
            // The theme's word for the state, not `label()` — that is the card's
            // title, which is already the last column.
            Theme::Command.state(&card.state),
            card.attempts,
            card.last_seq,
            card.label()
        );
        if let Some(question) = &card.question {
            println!("         asks: {question}");
        }
    }
    println!("-- the last 12 feed lines --");
    let feed_lines: Vec<_> = view.feed().iter().collect();
    for line in feed_lines.iter().rev().take(12).rev() {
        println!("  {}", line.text);
    }

    assert!(view.events() > 0, "the log is empty");
    assert!(!view.cards().is_empty(), "no task came out of the log");
}
