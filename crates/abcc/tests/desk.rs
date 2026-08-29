//! The console edge: the verbs an operator types, and the order they reach
//! things in.
//!
//! 🚨 ADR-0006 calls the ordering load-bearing: **the log first, the channel
//! second.** A verb that reaches the log and not the channel is still honoured by
//! boot replay; one that reaches the channel and not the log never happened. The
//! two tests that matter here are the ones that hold that apart — the row lands
//! whether or not there is anybody left to poke, and the poke is what a live
//! worker sees.

use std::path::Path;

use abcc::desk::{Delivered, Desk, parse_verb};
use abcc_core::event::{Control, Event};
use abcc_core::seq::{MissionId, Seq, TaskId};
use abcc_engine::control::{ControlPoint, Disposition, Keep};
use abcc_store::Store;

/// A log on disk with one task on it. On disk rather than in memory because the
/// claim being made is about **two connections to one file**.
fn seeded(path: &Path) -> TaskId {
    let mut store = Store::open(path).expect("open");
    let mission = store
        .append(Event::MissionCreated {
            title: "skeleton".into(),
        })
        .expect("mission");
    let task = store
        .append(Event::TaskCreated {
            mission: MissionId::at(mission.seq),
            title: "one returns two".into(),
            prompt: "make one() return two".into(),
        })
        .expect("task");
    TaskId::at(task.seq)
}

fn controls(path: &Path) -> Vec<Control> {
    Store::open(path)
        .expect("reopen")
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::ControlRequested { control, .. } => Some(control),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// the ordering
// ---------------------------------------------------------------------------

#[test]
fn a_verb_reaches_the_log_and_then_the_worker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let (mut point, handle) = ControlPoint::new();
    let mut desk = Desk::open(&log, handle, task).expect("desk");

    assert_eq!(
        desk.request(Control::Halt).expect("request"),
        Delivered::ToTheWorker
    );
    assert_eq!(controls(&log), vec![Control::Halt]);
    // And the worker sees it, which is the half the log cannot do on its own.
    let Disposition::Stop(stop) = point.check() else {
        panic!("the worker should have latched a stop");
    };
    assert_eq!(stop.control, Control::Halt);
    assert_eq!(stop.keep, Keep::AtCheckpoint);
}

#[test]
fn a_verb_is_written_even_when_there_is_no_longer_anybody_to_poke() {
    // 🚨 The ordering, made visible: the worker is gone before the verb is sent,
    // and the row still lands. If the poke came first and the write were
    // conditional on it, this log would be empty — and a verb that is not on the
    // log did not happen.
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let (point, handle) = ControlPoint::new();
    let mut desk = Desk::open(&log, handle, task).expect("desk");
    drop(point);

    assert_eq!(
        desk.request(Control::Kill).expect("request"),
        Delivered::LoggedOnly
    );
    assert_eq!(controls(&log), vec![Control::Kill]);
}

#[test]
fn the_desks_connection_writes_while_another_connection_holds_the_log() {
    // The two-writer claim, stated rather than assumed. The driver borrows a
    // `Store` mutably for the whole of an attempt, so honouring the ordering
    // above needs a second connection; `seq` is allocated by SQLite inside the
    // transaction and `busy_timeout` is set on both, which is what makes that
    // safe.
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let mut driver_side = Store::open(&log).expect("driver side");
    let (_point, handle) = ControlPoint::new();
    let mut desk = Desk::open(&log, handle, task).expect("desk");

    driver_side
        .append(Event::Note {
            text: "the attempt is running".into(),
        })
        .expect("note");
    desk.request(Control::Pause).expect("request");
    driver_side
        .append(Event::Note {
            text: "and it carried on".into(),
        })
        .expect("note");

    // One log, one ordering, and the desk's row is in the middle of the driver's
    // two rather than beside them.
    let kinds: Vec<&'static str> = driver_side
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .iter()
        .map(|l| l.event.kind())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "mission_created",
            "task_created",
            "note",
            "control_requested",
            "note"
        ]
    );
}

// ---------------------------------------------------------------------------
// the verbs
// ---------------------------------------------------------------------------

#[test]
fn the_five_verbs_parse_and_nothing_else_does() {
    assert_eq!(parse_verb("pause"), Some(Control::Pause));
    assert_eq!(parse_verb("halt"), Some(Control::Halt));
    assert_eq!(parse_verb("kill"), Some(Control::Kill));
    assert_eq!(parse_verb("resume"), Some(Control::Resume));
    assert_eq!(
        parse_verb("redirect look in src/lib.rs instead"),
        Some(Control::Redirect {
            prompt: "look in src/lib.rs instead".to_owned()
        })
    );

    for nonsense in ["", "   ", "stop", "abort", "pause now"] {
        assert!(
            !matches!(parse_verb(nonsense), Some(Control::Pause | Control::Halt)),
            "{nonsense:?} should not be a stop"
        );
    }
    assert_eq!(parse_verb("stop"), None);
}

#[test]
fn a_verb_is_recognised_however_it_is_typed() {
    assert_eq!(parse_verb("  HALT  "), Some(Control::Halt));
    assert_eq!(parse_verb("K"), Some(Control::Kill));
    assert_eq!(parse_verb("p"), Some(Control::Pause));
}

#[test]
fn a_redirect_with_no_prompt_is_not_a_redirect() {
    // The whole verb is the new question. Forking an attempt onto an empty one
    // would spend a slot to ask nothing.
    assert_eq!(parse_verb("redirect"), None);
    assert_eq!(parse_verb("redirect    "), None);
    assert_eq!(parse_verb("r"), None);
}

#[test]
fn a_redirects_prompt_keeps_its_own_spacing() {
    // It is a prompt, not a word list: only the ends are trimmed.
    assert_eq!(
        parse_verb("redirect   use  Repo::open  instead   "),
        Some(Control::Redirect {
            prompt: "use  Repo::open  instead".to_owned()
        })
    );
}
