//! The keys, and the one platform detail that would otherwise double them.

use abcc_core::event::Event;
use abcc_core::seq::{MissionId, Seq};
use abcc_tui::reader::{Intent, PAGE};
use abcc_tui::{Reader, Replay, Theme};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

fn notes(n: usize) -> Replay {
    let mut log = Replay::new();
    for i in 0..n {
        log.advance(1).push(Event::Note {
            text: format!("note {i}"),
        });
    }
    log
}

fn three_tasks() -> Replay {
    let mut log = Replay::new();
    let mission = MissionId::at(Seq::new(1));
    for i in 0..3 {
        log.advance(1).push(Event::TaskCreated {
            mission,
            title: format!("task {i}"),
            prompt: String::new(),
        });
    }
    log
}

fn press(reader: &mut Reader, log: &Replay, code: KeyCode) -> Intent {
    let intent = reader.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    reader.act(log, intent)
}

#[test]
fn the_position_keys_move_the_cursor_and_nothing_else() {
    let log = notes(4);
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    assert_eq!(reader.view().cursor(), Seq::new(4));

    assert_eq!(
        press(&mut reader, &log, KeyCode::Char('g')),
        Intent::Rewind(Seq::ORIGIN)
    );
    assert_eq!(reader.view().cursor(), Seq::ORIGIN);
    assert_eq!(reader.view().events(), 0);
    assert!(!reader.following(), "a scrub position is not the tail");

    assert_eq!(press(&mut reader, &log, KeyCode::Char('G')), Intent::Step);
    assert_eq!(reader.view().cursor(), Seq::new(4));
    assert!(reader.following());

    assert_eq!(
        press(&mut reader, &log, KeyCode::Char('f')),
        Intent::Nothing
    );
    assert!(!reader.following(), "f is a toggle");
}

#[test]
fn the_selection_moves_by_one_and_stops_at_the_ends() {
    let log = three_tasks();
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    assert_eq!(reader.selected(), 0);

    press(&mut reader, &log, KeyCode::Char('j'));
    assert_eq!(reader.selected(), 1);
    press(&mut reader, &log, KeyCode::Down);
    press(&mut reader, &log, KeyCode::Down);
    assert_eq!(reader.selected(), 2, "the board has three tasks");
    assert_eq!(reader.card().map(|c| c.title.clone()).unwrap(), "task 2");

    press(&mut reader, &log, KeyCode::Char('k'));
    press(&mut reader, &log, KeyCode::Up);
    press(&mut reader, &log, KeyCode::Up);
    assert_eq!(reader.selected(), 0);
}

#[test]
fn a_selection_past_the_end_survives_a_rewind() {
    // Rewinding throws the board away, so a selection that pointed at the third
    // task now points at nothing. It clamps rather than panicking or drawing an
    // empty footer for a task that exists.
    let log = three_tasks();
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    press(&mut reader, &log, KeyCode::Char('j'));
    press(&mut reader, &log, KeyCode::Char('j'));
    assert_eq!(reader.selected(), 2);

    reader.rewind(&log, Seq::new(1));
    assert_eq!(reader.selected(), 0);
    assert_eq!(reader.card().map(|c| c.title.clone()).unwrap(), "task 0");

    reader.rewind(&log, Seq::ORIGIN);
    assert_eq!(reader.card(), None);
}

#[test]
fn q_and_escape_are_the_way_out() {
    let log = notes(1);
    let mut reader = Reader::new(Theme::Command);
    assert_eq!(press(&mut reader, &log, KeyCode::Char('q')), Intent::Quit);
    assert_eq!(press(&mut reader, &log, KeyCode::Esc), Intent::Quit);
}

#[test]
fn a_page_key_moves_by_exactly_one_page() {
    let log = notes(PAGE + 40);
    let mut reader = Reader::new(Theme::Command);
    while reader.pump(&log) > 0 {}
    let head = reader.view().cursor();
    assert_eq!(head.get(), i64::try_from(PAGE + 40).unwrap());

    let page = i64::try_from(PAGE).unwrap();
    let back = press(&mut reader, &log, KeyCode::Char('['));
    assert_eq!(back, Intent::Rewind(Seq::new(head.get() - page)));
    assert_eq!(reader.view().cursor(), Seq::new(40));

    press(&mut reader, &log, KeyCode::Char(']'));
    assert_eq!(
        reader.view().cursor(),
        head,
        "one page forward reaches the tail"
    );
}

#[test]
fn paging_back_past_the_origin_stops_at_the_origin() {
    let log = notes(3);
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    press(&mut reader, &log, KeyCode::Char('['));
    assert_eq!(reader.view().cursor(), Seq::ORIGIN);
    assert_eq!(reader.view().events(), 0);
}

#[test]
fn a_key_release_does_nothing() {
    // ⚠ Windows reports a key event on release as well as on press. A reader that
    // did not filter would act on every keystroke twice, which for `[` means a
    // scrub jumping two pages for one press — a bug that would read as the paging
    // arithmetic being wrong rather than as the events being doubled.
    let log = notes(4);
    let mut reader = Reader::new(Theme::Command);
    reader.pump(&log);
    let release = KeyEvent::new_with_kind(
        KeyCode::Char('g'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    assert_eq!(reader.on_key(release), Intent::Nothing);
    assert_eq!(
        reader.view().cursor(),
        Seq::new(4),
        "a key release moved the cursor"
    );
}

#[test]
fn the_terminal_backend_and_the_key_events_are_one_crossterm() {
    // This crate names crossterm 0.29 and `ratatui`'s default features select
    // `crossterm_0_29`. If those ever resolve to two different crates the loop
    // would read key events of a type `on_key` cannot take, and the failure would
    // be a compile error a long way from its cause. The assignment below
    // type-checks only while they are one crate, so the cause is here instead.
    let ours: crossterm::event::KeyCode = KeyCode::Char('q');
    let theirs: ratatui::crossterm::event::KeyCode = ours;
    assert_eq!(theirs, ratatui::crossterm::event::KeyCode::Char('q'));
}
