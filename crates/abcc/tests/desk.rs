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

use abcc::desk::{
    Behind, Delivered, Desk, FleetDesk, GROUND_NOTE, Misread, Order, caveat, parse_order,
    parse_verb,
};
use abcc_core::event::{Control, Event};
use abcc_core::seq::{MissionId, Seq, TaskId};
use abcc_engine::control::{ControlPoint, Disposition, InFlight, Keep};
use abcc_fleet::StandDown;
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

// ---------------------------------------------------------------------------
// The fleet's desk: the same edge, across a slot that moves
// ---------------------------------------------------------------------------

/// A second task on the same log, so a verb has somewhere wrong to go.
fn also(path: &Path, title: &str) -> TaskId {
    let mut store = Store::open(path).expect("open");
    let mission = store
        .append(Event::MissionCreated {
            title: "second".into(),
        })
        .expect("mission");
    let task = store
        .append(Event::TaskCreated {
            mission: MissionId::at(mission.seq),
            title: title.into(),
            prompt: "and again".into(),
        })
        .expect("task");
    TaskId::at(task.seq)
}

/// Every `ControlRequested` on the log with the task it was written against.
fn addressed(path: &Path) -> Vec<(TaskId, Control)> {
    Store::open(path)
        .expect("reopen")
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::ControlRequested { task, control } => Some((task, control)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_fleet_verb_reaches_the_log_and_the_task_it_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let in_flight = InFlight::default();
    let (mut point, handle) = ControlPoint::new();
    in_flight.takes(task, handle);
    let mut desk = FleetDesk::open(&log, in_flight.clone(), StandDown::default()).expect("desk");

    assert_eq!(desk.flying(), Some(task));
    assert_eq!(
        desk.request(task, Control::Halt).expect("request"),
        Delivered::ToTheWorker
    );
    assert_eq!(addressed(&log), vec![(task, Control::Halt)]);
    let Disposition::Stop(stop) = point.check() else {
        panic!("the worker should have latched a stop");
    };
    assert_eq!(stop.control, Control::Halt);
}

/// 🚨🚨 **The reason this type exists.** The operator was looking at the first
/// task when they started typing; by the time they pressed enter the slot had
/// moved to the second. The verb is written down against the task they **named**
/// and delivered to nothing — and, above all, the second task is untouched.
///
/// ⚠ The alternative — re-aiming the verb at whatever is flying — is what a desk
/// bound to *the task in flight* does, and it is a `kill` arriving at work nobody
/// looked at.
#[test]
fn a_verb_for_a_task_the_slot_is_not_on_is_logged_and_sent_nowhere() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let first = seeded(&log);
    let second = also(&log, "and again");

    let in_flight = InFlight::default();
    let (mut running, handle) = ControlPoint::new();
    // The slot has moved on.
    in_flight.takes(second, handle);
    let mut desk = FleetDesk::open(&log, in_flight, StandDown::default()).expect("desk");

    assert_eq!(
        desk.request(first, Control::Kill).expect("request"),
        Delivered::NotInFlight {
            flying: Some(second)
        }
    );

    // On the log, against the task the operator named. Never against the one
    // that happened to be flying — a record that silently re-addressed itself
    // would be worse than no record.
    assert_eq!(addressed(&log), vec![(first, Control::Kill)]);
    assert_eq!(
        running.check(),
        Disposition::Carry,
        "the task the operator was not looking at took the verb"
    );
}

/// Between attempts the slot is empty, and a verb then is still written down.
#[test]
fn a_verb_between_attempts_is_logged_and_says_the_slot_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let in_flight = InFlight::default();
    let (_point, handle) = ControlPoint::new();
    in_flight.takes(task, handle);
    in_flight.released();
    let mut desk = FleetDesk::open(&log, in_flight, StandDown::default()).expect("desk");

    assert_eq!(desk.flying(), None);
    assert_eq!(
        desk.request(task, Control::Pause).expect("request"),
        Delivered::NotInFlight { flying: None }
    );
    assert_eq!(addressed(&log), vec![(task, Control::Pause)]);
}

/// The right task, and its worker finished between the keystroke and the send.
/// The ordinary race, and it is told apart from naming the wrong task.
#[test]
fn a_verb_for_the_flying_task_whose_worker_has_ended_is_logged_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    let task = seeded(&log);

    let in_flight = InFlight::default();
    let (point, handle) = ControlPoint::new();
    in_flight.takes(task, handle);
    drop(point);
    let mut desk = FleetDesk::open(&log, in_flight, StandDown::default()).expect("desk");

    assert_eq!(
        desk.request(task, Control::Kill).expect("request"),
        Delivered::LoggedOnly
    );
    assert_eq!(addressed(&log), vec![(task, Control::Kill)]);
}

/// `ground` is on the log before it is in force, the same way round as a verb.
#[test]
fn a_stand_down_reaches_the_log_and_then_the_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.sqlite");
    seeded(&log);

    let stand_down = StandDown::default();
    let mut desk = FleetDesk::open(&log, InFlight::default(), stand_down.clone()).expect("desk");
    assert!(!stand_down.ordered());

    desk.ground().expect("ground");
    assert!(stand_down.ordered());

    // ⚠ A `Note`, because a stand-down has no task and `ControlRequested` is the
    // event that does.
    let notes: Vec<String> = Store::open(&log)
        .expect("reopen")
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::Note { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(notes, vec![GROUND_NOTE.to_owned()]);
    assert!(addressed(&log).is_empty());
}

// ---------------------------------------------------------------------------
// The orders, as an operator types them
// ---------------------------------------------------------------------------

fn t(n: i64) -> TaskId {
    TaskId::at(Seq::new(n))
}

#[test]
fn every_task_verb_takes_a_task_and_the_two_sortie_orders_take_nothing() {
    assert_eq!(
        parse_order("pause t42"),
        Ok(Order::ToTask {
            task: t(42),
            control: Control::Pause
        })
    );
    assert_eq!(
        parse_order("k 7"),
        Ok(Order::ToTask {
            task: t(7),
            control: Control::Kill
        })
    );
    assert_eq!(
        parse_order("  HALT  t3 "),
        Ok(Order::ToTask {
            task: t(3),
            control: Control::Halt
        })
    );
    // ⚠ The verb case-folds and the task id does not, because the id has to be
    // the spelling `abcc accept t3` already takes — an operator learns one, and a
    // desk that quietly accepted a second would be teaching them the wrong one.
    assert_eq!(
        parse_order("HALT T3"),
        Err(Misread::NotATask("T3".to_owned()))
    );
    assert_eq!(
        parse_order("resume t9"),
        Ok(Order::ToTask {
            task: t(9),
            control: Control::Resume
        })
    );
    assert_eq!(
        parse_order("redirect t42 look in src/lib.rs instead"),
        Ok(Order::ToTask {
            task: t(42),
            control: Control::Redirect {
                prompt: "look in src/lib.rs instead".to_owned()
            }
        })
    );
    assert_eq!(parse_order("ground"), Ok(Order::Ground));
    assert_eq!(parse_order("slot"), Ok(Order::Slot));
}

/// 🚨 **A bare verb is refused, and that is the design rather than a gap.** At a
/// fleet the slot moves, so a verb with no name would be delivered to whatever
/// was flying when the operator pressed enter.
#[test]
fn a_verb_with_no_task_is_refused_and_says_why() {
    for bare in ["pause", "halt", "kill", "resume", "redirect", "p", "r"] {
        assert_eq!(
            parse_order(bare),
            Err(Misread::Unaddressed(bare.to_owned())),
            "{bare:?} was accepted without a task"
        );
    }
    // And the sentence names the fix rather than the rule.
    let complaint = Misread::Unaddressed("kill".to_owned()).to_string();
    assert!(complaint.contains("kill t42"), "{complaint}");
    assert!(complaint.contains("slot"), "{complaint}");
}

#[test]
fn a_task_that_is_not_a_task_id_is_refused_as_itself() {
    assert_eq!(
        parse_order("kill everything"),
        Err(Misread::NotATask("everything".to_owned()))
    );
    assert_eq!(
        parse_order("pause t0"),
        Err(Misread::NotATask("t0".to_owned()))
    );
    assert_eq!(
        parse_order("pause t-1"),
        Err(Misread::NotATask("t-1".to_owned()))
    );
}

#[test]
fn the_verbs_that_take_nothing_still_accept_nothing() {
    assert_eq!(
        parse_order("pause t42 now"),
        Err(Misread::Trailing {
            order: "pause".to_owned(),
            extra: "now".to_owned()
        })
    );
    assert_eq!(
        parse_order("ground now"),
        Err(Misread::Trailing {
            order: "ground".to_owned(),
            extra: "now".to_owned()
        })
    );
    assert_eq!(
        parse_order("slot 3"),
        Err(Misread::Trailing {
            order: "slot".to_owned(),
            extra: "3".to_owned()
        })
    );
}

#[test]
fn a_redirect_with_a_task_and_no_prompt_is_not_a_redirect() {
    assert_eq!(parse_order("redirect t42"), Err(Misread::EmptyRedirect));
    assert_eq!(parse_order("r t42    "), Err(Misread::EmptyRedirect));
    // ⚠ And a redirect with a prompt and no task is unaddressed, not empty: the
    // operator wrote a question and left off who it was for.
    assert_eq!(
        parse_order("redirect look over there"),
        Err(Misread::NotATask("look".to_owned()))
    );
}

#[test]
fn a_redirects_prompt_keeps_its_own_spacing_at_a_fleet_too() {
    assert_eq!(
        parse_order("redirect t42   use  Repo::open  instead   "),
        Ok(Order::ToTask {
            task: t(42),
            control: Control::Redirect {
                prompt: "use  Repo::open  instead".to_owned()
            }
        })
    );
}

#[test]
fn nothing_else_is_an_order() {
    for nonsense in ["stop", "abort", "ground t42", "flying"] {
        assert!(
            !matches!(parse_order(nonsense), Ok(Order::ToTask { .. })),
            "{nonsense:?} became a verb"
        );
    }
    assert_eq!(
        parse_order("stop t42"),
        Err(Misread::NotAnOrder("stop t42".to_owned()))
    );
}

/// ⚠ The two desks read the same table. `parse_verb` is the run's and takes no
/// task; `parse_order` is the fleet's and requires one — and a word that means
/// `kill` at one has to mean `kill` at the other.
#[test]
fn the_two_desks_agree_about_what_a_word_means() {
    for (word, control) in [
        ("pause", Control::Pause),
        ("p", Control::Pause),
        ("halt", Control::Halt),
        ("h", Control::Halt),
        ("kill", Control::Kill),
        ("k", Control::Kill),
        ("resume", Control::Resume),
    ] {
        assert_eq!(parse_verb(word), Some(control.clone()), "run: {word}");
        assert_eq!(
            parse_order(&format!("{word} t42")),
            Ok(Order::ToTask {
                task: t(42),
                control
            }),
            "fleet: {word}"
        );
    }
}

/// 🚨 **F646: all five verbs have a mechanism at a sortie, and two of them still
/// have none at `abcc run` — because there is no loop after the attempt.**
///
/// The test that stood here was the tripwire on ADR-0012 §4's *eight verbs that
/// half-work are worse than three that work*, and it said in its own words: *the
/// day `resume` is wired, it fails, and the line telling operators it is not
/// wired has to go with it.* It fired. What replaces it asks the same sentence
/// **per desk**, so the day the one-attempt path grows a loop this fails the same
/// way rather than quietly going stale.
#[test]
fn every_verb_is_wired_at_a_sortie_and_two_are_not_at_a_run() {
    let all_five = || {
        [
            Control::Pause,
            Control::Halt,
            Control::Kill,
            Control::Resume,
            Control::Redirect {
                prompt: "look elsewhere".to_owned(),
            },
        ]
    };

    for control in all_five() {
        assert_eq!(
            caveat(&control, Behind::ASortie),
            None,
            "{control:?} still owes a sortie a caveat"
        );
    }
    for control in [Control::Pause, Control::Halt, Control::Kill] {
        assert_eq!(
            caveat(&control, Behind::OneAttempt),
            None,
            "{control:?} grew a caveat"
        );
    }

    let redirect = caveat(
        &Control::Redirect {
            prompt: "look elsewhere".to_owned(),
        },
        Behind::OneAttempt,
    )
    .expect("a run ends after the attempt, so it forks nothing");
    let resume = caveat(&Control::Resume, Behind::OneAttempt)
        .expect("a run has no admission loop to resume into");

    // 🚨 The point of both sentences is what to do next, not that nothing
    // happened: the row **is** on the log, and a sortie is what reads it. A
    // caveat that only said "inert" would leave the operator thinking the verb
    // was lost.
    for line in [redirect, resume] {
        assert!(line.contains("abcc fleet"), "{line:?} names no way forward");
        // ⚠ The warning sign is a character in a string literal, not the six
        // characters of an escape nothing ever read. F558 is that mistake, made
        // in a comment; this is the same check one line from where it would
        // happen.
        assert!(line.contains('\u{26a0}'), "{line:?} lost its mark");
        assert!(!line.contains("\\u{"), "{line:?} carries a dead escape");
    }
}
