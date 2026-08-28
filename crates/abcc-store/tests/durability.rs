//! What the log promises, asserted rather than described.
//!
//! The Skeleton milestone's exit criterion is here in two tests:
//! [`boot_reconstructs_identical_status_from_the_log_alone`] and
//! [`killing_the_process_mid_attempt_requeues_and_tombstones_it`]. The rest are
//! the invariants those two depend on.

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::Event;
use abcc_core::outcome::Why;
use abcc_core::run::{DowngradeReason, Mode};
use abcc_core::seq::{AttemptId, MissionId, Seq, TaskId, UnitId};
use abcc_core::task::{AbortReason, Command, TaskState};
use abcc_store::{Applied, Store, StoreError, TaskRow};

/// A mission with one task on it, which is the smallest board worth booting.
fn seed(store: &mut Store) -> (MissionId, TaskId) {
    let m = store
        .append(Event::MissionCreated {
            title: "skeleton".into(),
        })
        .expect("mission");
    let mission = MissionId::at(m.seq);
    let t = store
        .append(Event::TaskCreated {
            mission,
            title: "make the reader show a run".into(),
            prompt: "read the event log and render it".into(),
        })
        .expect("task");
    (mission, TaskId::at(t.seq))
}

/// Drive a task to `Engaged`, returning the attempt that is now in flight.
fn engage(store: &mut Store, task: TaskId) -> AttemptId {
    let moved = store
        .apply(task, Command::Deploy { unit: UnitId(0) })
        .expect("deploy");
    assert!(moved.moved());

    let started = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: Cause::Fresh,
            checkpoint_from: None,
        })
        .expect("attempt started");
    let attempt = AttemptId::at(started.seq);

    let moved = store
        .apply(task, Command::Engage { attempt })
        .expect("engage");
    assert!(moved.moved());
    attempt
}

fn board(store: &Store) -> Vec<TaskRow> {
    store.tasks().expect("tasks")
}

// ---------------------------------------------------------------------------
// The exit criterion
// ---------------------------------------------------------------------------

/// 🚨 Skeleton's exit criterion, first half: **the status after a restart is the
/// status before it, reconstructed from the log and nothing else.**
///
/// The projection is deleted outright before the rebuild, so there is no way for
/// a stale row to be mistaken for a recovered one.
#[test]
fn boot_reconstructs_identical_status_from_the_log_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("run.sqlite");

    let before = {
        let mut store = Store::open(&path).expect("open");
        let (_, task) = seed(&mut store);
        let attempt = engage(&mut store, task);
        store
            .append(Event::AttemptEnded {
                task,
                attempt,
                outcome: AttemptOutcome::Success,
            })
            .expect("attempt ended");
        store
            .apply(task, Command::Accomplish { attempt })
            .expect("accomplish");
        board(&store)
    };
    assert_eq!(before.len(), 1);
    assert!(matches!(before[0].state, TaskState::Accomplished { .. }));

    // A second process opens the same file and replays.
    let mut store = Store::open(&path).expect("reopen");
    let n = store.rebuild().expect("rebuild");
    assert!(n > 0, "the log was empty");
    assert_eq!(board(&store), before, "the board did not survive a restart");
}

/// 🚨 Skeleton's exit criterion, second half: **kill the process mid-attempt and
/// the restart tells the truth about it.**
///
/// v1's failure here is F151's: a slot-holding state with no live worker behind
/// it, and an attempt row that stays open forever because the only thing that
/// could have closed it was the process that died. The task comes back to
/// `Queued` and the attempt is tombstoned — not resumed, because attempts are
/// immutable and half of one is not a thing that can be continued.
#[test]
fn killing_the_process_mid_attempt_requeues_and_tombstones_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("run.sqlite");

    let (task, attempt) = {
        let mut store = Store::open(&path).expect("open");
        let (_, task) = seed(&mut store);
        let attempt = engage(&mut store, task);
        // No AttemptEnded, no terminal transition. This is the process dying.
        (task, attempt)
    };

    let mut store = Store::open(&path).expect("reopen");

    // Before reconciliation, the log says exactly what was true when it stopped.
    store.rebuild().expect("rebuild");
    assert!(matches!(
        store.task(task).expect("task").expect("row").state,
        TaskState::Engaged { .. }
    ));

    let reconciled = store.boot().expect("boot");
    assert_eq!(reconciled.requeued, vec![task]);
    assert_eq!(
        store.task(task).expect("task").expect("row").state,
        TaskState::Queued
    );

    let a = store.attempt(attempt).expect("attempt").expect("row");
    assert!(!a.is_open(), "the orphaned attempt is still open");
    assert!(matches!(
        a.outcome,
        Some(AttemptOutcome::HardFailure {
            why: Why::EngineError { .. }
        })
    ));
}

/// Booting twice over the same log does nothing the second time. If it did, the
/// reconciliation would be writing state rather than recording a fact, and the
/// log would grow every time the operator restarted.
#[test]
fn a_second_boot_over_the_same_log_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("run.sqlite");
    {
        let mut store = Store::open(&path).expect("open");
        let (_, task) = seed(&mut store);
        engage(&mut store, task);
    }

    let mut store = Store::open(&path).expect("reopen");
    let first = store.boot().expect("boot 1");
    assert_eq!(first.requeued.len(), 1);
    let head_after_first = store.head().expect("head");

    let second = store.boot().expect("boot 2");
    assert!(
        second.requeued.is_empty(),
        "the second boot moved something"
    );
    assert_eq!(
        store.head().expect("head"),
        head_after_first,
        "the second boot wrote to the log"
    );
}

/// `AwaitingOrders` is re-presented, never answered, and the task does not move.
/// The arrival time is part of the contract, so a restart that silently resolved
/// the question would destroy the thing being measured.
#[test]
fn boot_re_presents_a_prompt_and_never_answers_it() {
    let mut store = Store::in_memory().expect("store");
    let (_, task) = seed(&mut store);
    let attempt = engage(&mut store, task);

    let prompted = store
        .append(Event::OperatorPrompted {
            task,
            attempt,
            question: "the tests reference a fixture that is not in the tree. Add it?".into(),
        })
        .expect("prompt");
    store
        .apply(
            task,
            Command::RequestOrders {
                attempt,
                prompt: abcc_core::seq::PromptId::at(prompted.seq),
            },
        )
        .expect("request orders");

    let reconciled = store.boot().expect("boot");
    assert_eq!(reconciled.prompts_to_represent, vec![task]);
    assert!(reconciled.requeued.is_empty());
    assert!(matches!(
        store.task(task).expect("task").expect("row").state,
        TaskState::AwaitingOrders { .. }
    ));
}

// ---------------------------------------------------------------------------
// The invariants underneath it
// ---------------------------------------------------------------------------

/// ADR-0005's pragmas, read back from the connection rather than assumed from
/// the code that set them. `synchronous=FULL` is 2, and it is the one place this
/// design overrides received practice, so it is worth an assertion.
#[test]
fn the_pragmas_are_the_ones_adr_0005_specifies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("run.sqlite");
    let store = Store::open(&path).expect("open");

    assert_eq!(store.pragma_text("journal_mode").expect("journal"), "wal");
    assert_eq!(store.pragma_int("synchronous").expect("synchronous"), 2);
    assert_eq!(store.pragma_int("foreign_keys").expect("fk"), 1);
    assert_eq!(store.pragma_int("busy_timeout").expect("busy"), 5000);
}

/// A refusal is a normal outcome: the task is untouched and the attempt to move
/// it is on the record. Foreign processes send commands that can be refused,
/// never column values that cannot.
#[test]
fn a_refused_command_leaves_the_task_alone_and_says_so_on_the_log() {
    let mut store = Store::in_memory().expect("store");
    let (_, task) = seed(&mut store);

    // Engage without deploying: there is no slot and no attempt.
    let applied = store
        .apply(
            task,
            Command::Engage {
                attempt: AttemptId::at(Seq::new(1)),
            },
        )
        .expect("apply");
    let Applied::Refused { logged, refusal } = applied else {
        panic!("the command was not refused")
    };
    assert!(refusal.to_string().contains("Queued"), "{refusal}");
    assert!(matches!(logged.event, Event::CommandRefused { .. }));
    assert_eq!(
        store.task(task).expect("task").expect("row").state,
        TaskState::Queued
    );
}

/// The projection is a cache with no cache miss: delete every row of it and the
/// board comes back identical, because the log never needed it.
#[test]
fn the_projection_is_rebuildable_and_is_not_the_source_of_truth() {
    let mut store = Store::in_memory().expect("store");
    let (_, task) = seed(&mut store);
    let attempt = engage(&mut store, task);
    store
        .append(Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Success,
        })
        .expect("ended");
    store
        .apply(task, Command::Accomplish { attempt })
        .expect("accomplish");

    let before = board(&store);
    let head_before = store.head().expect("head");

    store.rebuild().expect("rebuild");

    assert_eq!(board(&store), before);
    assert_eq!(
        store.head().expect("head"),
        head_before,
        "the rebuild wrote to the log"
    );
}

/// A command on a task the projection does not know is an error rather than a
/// silent no-op, and it leaves nothing behind — no reserved row, no gap.
#[test]
fn a_write_that_cannot_land_leaves_no_trace() {
    let mut store = Store::in_memory().expect("store");
    let (_, task) = seed(&mut store);
    let head_before = store.head().expect("head");

    let err = store
        .apply(
            TaskId::at(Seq::new(9999)),
            Command::Deploy { unit: UnitId(0) },
        )
        .expect_err("a command on an unknown task was accepted");
    assert!(matches!(err, StoreError::NoSuchTask(_)), "{err}");
    assert_eq!(store.head().expect("head"), head_before);

    // And the real task is still usable afterwards.
    assert!(
        store
            .apply(task, Command::Deploy { unit: UnitId(0) })
            .expect("deploy")
            .moved()
    );
}

/// An attempt is immutable, so ending one twice is a caller bug and is refused
/// by the projection rather than absorbed. v1's four retry mechanisms all mutate
/// the row they are handed, which is how lineage was lost.
#[test]
fn an_attempt_cannot_end_twice() {
    let mut store = Store::in_memory().expect("store");
    let (_, task) = seed(&mut store);
    let attempt = engage(&mut store, task);

    store
        .append(Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Success,
        })
        .expect("first ending");

    let err = store
        .append(Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::HardFailure {
                why: Why::EngineError {
                    detail: "second thoughts".into(),
                },
            },
        })
        .expect_err("an attempt ended twice");
    assert!(matches!(err, StoreError::Unprojectable { .. }), "{err}");
}

/// `seq` is dense and strictly increasing on the happy path. The reservation the
/// writer takes before deciding what to write does not leave holes, because it
/// lives inside the transaction that either completes it or rolls it back.
#[test]
fn seq_is_dense_and_strictly_increasing() {
    let mut store = Store::in_memory().expect("store");
    let (_, task) = seed(&mut store);
    engage(&mut store, task);
    store
        .apply(
            task,
            Command::Abort {
                reason: AbortReason::Operator { by: "david".into() },
            },
        )
        .expect("abort");

    let all = store.read_from(Seq::ORIGIN, 1000).expect("read");
    assert!(all.len() >= 5);
    for (i, l) in all.iter().enumerate() {
        assert_eq!(
            l.seq.get(),
            i64::try_from(i).unwrap() + 1,
            "a hole or a repeat at index {i}"
        );
    }
    assert_eq!(store.head().expect("head"), all.last().unwrap().seq);
}

/// The read path is a cursor: the same integer is the event id, the resume
/// position and the page boundary.
#[test]
fn read_from_pages_by_the_same_integer_the_events_are_identified_by() {
    let mut store = Store::in_memory().expect("store");
    let (_, task) = seed(&mut store);
    engage(&mut store, task);

    let all = store.read_from(Seq::ORIGIN, 1000).expect("all");
    let mut paged = Vec::new();
    let mut cursor = Seq::ORIGIN;
    loop {
        let page = store.read_from(cursor, 2).expect("page");
        if page.is_empty() {
            break;
        }
        cursor = page.last().unwrap().seq;
        paged.extend(page);
    }
    assert_eq!(paged, all);
}

/// One task's history is reachable without scanning the whole log, and it
/// contains that task's events and no other task's.
#[test]
fn a_task_history_is_that_task_and_nothing_else() {
    let mut store = Store::in_memory().expect("store");
    let (mission, first) = seed(&mut store);
    let second = TaskId::at(
        store
            .append(Event::TaskCreated {
                mission,
                title: "the other one".into(),
                prompt: "unrelated".into(),
            })
            .expect("task")
            .seq,
    );
    engage(&mut store, first);
    engage(&mut store, second);

    let h = store.task_history(first).expect("history");
    assert!(!h.is_empty());
    for l in &h {
        assert_eq!(l.event.task(), Some(first), "{:?}", l.event);
    }
}

/// The effective mode is a projection over the log, not a variable. ADR-0013's
/// ratchet is one-way and the downgrade is the event that records why.
#[test]
fn the_effective_mode_is_a_projection_of_the_log() {
    let mut store = Store::in_memory().expect("store");
    assert_eq!(store.effective_mode().expect("mode"), None);

    store
        .append(Event::RunStarted {
            mode: Mode::CoOp,
            version: "0.1.0".into(),
            pid: 1234,
        })
        .expect("run started");
    assert_eq!(store.effective_mode().expect("mode"), Some(Mode::CoOp));

    store
        .append(Event::ModeDowngraded {
            from: Mode::CoOp,
            to: Mode::SinglePlayer,
            why: DowngradeReason::NetworkFailure {
                detail: "the cloud provider stopped answering".into(),
            },
        })
        .expect("downgrade");
    assert_eq!(
        store.effective_mode().expect("mode"),
        Some(Mode::SinglePlayer)
    );

    // And it survives a rebuild, because it was never held anywhere else.
    store.rebuild().expect("rebuild");
    assert_eq!(
        store.effective_mode().expect("mode"),
        Some(Mode::SinglePlayer)
    );
}
