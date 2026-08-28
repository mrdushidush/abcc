//! What the journal does with a write that did not land.
//!
//! [`abcc_engine::turn::Journal::record`] returns nothing and `Store::append`
//! can fail, so this is the one place in the workspace where a durable write has
//! nowhere to report to. The rule is that it is **latched and reported, never
//! swallowed** — and that the events after it are still written, because a hole
//! in the history is invisible and an error is not.

use abcc_core::attempt::AttemptOutcome;
use abcc_core::event::Event;
use abcc_core::outcome::Why;
use abcc_core::seq::{AttemptId, Seq, TaskId};
use abcc_drive::StoreJournal;
use abcc_engine::turn::Journal;
use abcc_store::{Store, StoreError};

/// An attempt id that names nothing. Ending it cannot be projected, which is
/// `abcc-store` refusing to let an attempt be ended twice or out of nowhere.
fn nonexistent() -> AttemptId {
    AttemptId::at(Seq::new(9_999))
}

#[test]
fn a_write_that_did_not_land_is_latched_and_reported() {
    let mut store = Store::in_memory().expect("store");
    let mut journal = StoreJournal::new(&mut store);

    journal.record(Event::AttemptEnded {
        task: TaskId::at(Seq::new(1)),
        attempt: nonexistent(),
        outcome: AttemptOutcome::Uncertain {
            why: Why::EngineError {
                detail: "made up".into(),
            },
        },
    });

    assert!(
        matches!(journal.failed(), Some(StoreError::Unprojectable { .. })),
        "a failed write was swallowed"
    );
    assert_eq!(journal.written(), 0);
    assert!(journal.into_result().is_err());
}

/// 🚨 The events *after* a failed write still land. Dropping them would turn one
/// bad write into a hole in the history, and the whole point of latching the
/// error is that the hole is reported instead of hidden.
#[test]
fn the_events_after_a_failed_write_are_still_written() {
    let mut store = Store::in_memory().expect("store");
    {
        let mut journal = StoreJournal::new(&mut store);
        journal.record(Event::AttemptEnded {
            task: TaskId::at(Seq::new(1)),
            attempt: nonexistent(),
            outcome: AttemptOutcome::Success,
        });
        journal.record(Event::Note {
            text: "what happened next".into(),
        });
        assert_eq!(journal.written(), 1);
        assert!(journal.into_result().is_err());
    }

    let notes: Vec<String> = store
        .read_from(Seq::ORIGIN, 100)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::Note { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(notes, vec!["what happened next".to_owned()]);
}

/// The ordinary case, so the failure tests above are not the only evidence the
/// type works.
#[test]
fn every_event_that_lands_is_counted() {
    let mut store = Store::in_memory().expect("store");
    let mut journal = StoreJournal::new(&mut store);
    for i in 0..5 {
        journal.record(Event::Note {
            text: format!("note {i}"),
        });
    }
    assert_eq!(journal.into_result().expect("no failures"), 5);
}
