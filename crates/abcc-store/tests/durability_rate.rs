//! What the write path actually costs on this machine.
//!
//! ADR-0005 chose `synchronous=FULL` over the WAL convention of `NORMAL` on an
//! arithmetic argument: 929 µs per transition against ~13–14 ms for a single
//! decoded token, on a log written at action granularity where one event covers
//! hundreds of tokens. That measurement was taken with CPython's `sqlite3`, and
//! the ADR says so and calls the rates **a floor** — interpreter overhead
//! inflates the cheap rows and is invisible in the fsync-bound ones.
//!
//! This is the same shape on the real path, so the floor can be checked rather
//! than inherited. It is `#[ignore]`d because it writes to disk and fsyncs a few
//! thousand times, which does not belong in the edit loop:
//!
//! ```text
//! cargo test -p abcc-store --test durability_rate -- --ignored --nocapture
//! ```

use std::time::Instant;

use abcc_core::event::Event;
use abcc_core::seq::{MissionId, TaskId, UnitId};
use abcc_core::task::{Command, RequeueReason};
use abcc_store::Store;

const N: usize = 500;

#[test]
#[ignore = "writes and fsyncs; run explicitly"]
fn measure_the_transition_rate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rate.sqlite");
    let mut store = Store::open(&path).expect("open");

    assert_eq!(
        store.pragma_text("journal_mode").expect("journal"),
        "wal",
        "not measuring what this test claims to measure"
    );
    assert_eq!(store.pragma_int("synchronous").expect("sync"), 2);

    let mission = MissionId::at(
        store
            .append(Event::MissionCreated {
                title: "rate".into(),
            })
            .expect("mission")
            .seq,
    );
    let task = TaskId::at(
        store
            .append(Event::TaskCreated {
                mission,
                title: "rate".into(),
                prompt: "rate".into(),
            })
            .expect("task")
            .seq,
    );

    // A transition is an event row appended and the status projection upserted
    // in the same transaction — the same shape the CPython bench used. Deploy
    // and requeue cycle without ever reaching a terminal state.
    let start = Instant::now();
    for _ in 0..N {
        store
            .apply(task, Command::Deploy { unit: UnitId(0) })
            .expect("deploy");
        store
            .apply(
                task,
                Command::Requeue {
                    why: RequeueReason::OrphanedByRestart,
                },
            )
            .expect("requeue");
    }
    let elapsed = start.elapsed();

    let transitions = N * 2;
    #[allow(clippy::cast_precision_loss)]
    let per_second = transitions as f64 / elapsed.as_secs_f64();
    #[allow(clippy::cast_precision_loss)]
    let micros = elapsed.as_secs_f64() * 1e6 / transitions as f64;
    println!("{transitions} transitions in {elapsed:?}: {per_second:.0}/s, {micros:.0} us each");

    let size = std::fs::metadata(&path).expect("stat").len();
    let head = store.head().expect("head").get();
    #[allow(clippy::cast_precision_loss)]
    let bytes_per_event = size as f64 / head as f64;
    println!("{head} events, {size} bytes on disk, {bytes_per_event:.0} bytes/event");

    // The floor the ADR quotes, with room for a slower disk. If this ever fires,
    // the pragma choice is the thing to re-argue — with a number.
    assert!(
        per_second > 100.0,
        "the write path is far below the measured floor: {per_second:.0}/s"
    );
}
