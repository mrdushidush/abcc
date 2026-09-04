//! `abcc fun` over a real log on disk.
//!
//! `crates/abcc-core/tests/fun.rs` folds the six over hand-built `Logged`
//! values, which proves the arithmetic and nothing about the log. This proves
//! the other half: that the events **survive the store** — written as JSON,
//! read back through `read_from`, matched on by the fold — and that the query
//! counts them on the far side.
//!
//! 🚨 **The agency query is the one that needs this.** It reads `0` on this
//! project's own log, and a zero from an instrument nobody has validated is not
//! a measurement. So this test is the positive control: a `ControlRequested` is
//! put on a real log, and the query has to find it. The live sortie that first
//! produced one (2026-09-04, F582–F586) ran against a scratch `--home` that no
//! longer exists, so its log is not available to stand in for this.

use std::path::Path;

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::{Control, Event, Logged};
use abcc_core::fun::{Answer, Fun, Verdict};
use abcc_core::outcome::Why;
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, MissionId, Seq, TaskId, UnitId};
use abcc_core::task::Command;
use abcc_store::Store;

/// Everything on the log, in `seq` order — the same paged read `abcc fun` does.
fn read_all(path: &Path) -> Vec<Logged> {
    let store = Store::open(path).expect("reopen");
    let mut all = Vec::new();
    let mut since = Seq::ORIGIN;
    loop {
        let page = store.read_from(since, 512).expect("read");
        let Some(last) = page.last() else {
            return all;
        };
        since = last.seq;
        all.extend(page);
    }
}

/// An attempt the store will accept an ending for. The store enforces that an
/// `AttemptEnded` names an attempt that was started, which is why the fixture
/// cannot skip this.
fn an_attempt(store: &mut Store, task: TaskId) -> AttemptId {
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
        .expect("attempt started");
    AttemptId::at(started.seq)
}

fn a_task(store: &mut Store) -> TaskId {
    let mission = store
        .append(Event::MissionCreated {
            title: "fun".into(),
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

/// 🚨 The positive control for the agency query. Without it, `0 control events`
/// is a number nobody has ever seen the other side of.
#[test]
fn a_control_verb_survives_the_store_and_the_query_counts_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("log.sqlite");
    let mut store = Store::open(&path).expect("open");
    let task = a_task(&mut store);
    store
        .append(Event::RunStarted {
            mode: Mode::SinglePlayer,
            version: "0.1.0".into(),
            pid: 4242,
        })
        .expect("run started");
    store
        .append(Event::ControlRequested {
            task,
            control: Control::Halt,
        })
        .expect("the verb");
    store
        .append(Event::LivenessMark {
            attempt: AttemptId::at(Seq::new(3)),
            note: "acknowledged".into(),
        })
        .expect("liveness");
    drop(store);

    let fun = Fun::over(&read_all(&path));
    assert_eq!(fun.runs, 1);
    assert_eq!(
        fun.agency.events, 1,
        "a control verb written to a real log must be found on the way back"
    );
    assert_eq!(fun.agency.runs_with_control, 1);
    assert_eq!(
        fun.agency.verdict(),
        Verdict::Held,
        "the control bar was touched, so it is not decoration"
    );
    // The two events belonging to no run are still counted as such.
    assert_eq!(fun.loose, 2);
}

/// The other side of the same control: an identical log with no verb on it
/// reads `Broken`, so the `Held` above is the verb and not the setup.
#[test]
fn the_same_log_without_a_verb_reads_broken() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("log.sqlite");
    let mut store = Store::open(&path).expect("open");
    a_task(&mut store);
    store
        .append(Event::RunStarted {
            mode: Mode::SinglePlayer,
            version: "0.1.0".into(),
            pid: 4242,
        })
        .expect("run started");
    store
        .append(Event::LivenessMark {
            attempt: AttemptId::at(Seq::new(3)),
            note: "alive".into(),
        })
        .expect("liveness");
    drop(store);

    let fun = Fun::over(&read_all(&path));
    assert_eq!(fun.agency.events, 0);
    assert_eq!(fun.agency.verdict(), Verdict::Broken);
}

/// A failed ending survives the round trip too — `AttemptOutcome` goes through
/// JSON, and the denominator of the honest-failure query depends on reading it
/// back as the same variant.
#[test]
fn a_failed_ending_survives_the_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("log.sqlite");
    let mut store = Store::open(&path).expect("open");
    let task = a_task(&mut store);
    store
        .append(Event::RunStarted {
            mode: Mode::SinglePlayer,
            version: "0.1.0".into(),
            pid: 4242,
        })
        .expect("run started");
    let attempt = an_attempt(&mut store, task);
    store
        .append(Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Refused {
                rung: "cargo test".into(),
                detail: "1 failed".into(),
            },
        })
        .expect("ending");
    drop(store);

    let fun = Fun::over(&read_all(&path));
    assert_eq!(fun.honest_failure.failed_runs, 1);
    // 🚨 And still no instrument for the numerator, so the ratio is not invented.
    assert!(matches!(
        fun.honest_failure.replayed,
        Answer::NoInstrument(_)
    ));
    assert_eq!(fun.honest_failure.verdict(), Verdict::Unknown);
}

/// An `Uncertain` ending written to a real log is read back as an absence, not
/// as a failure. The distinction is the one thing `AttemptOutcome` exists to
/// carry, and it has to survive serialization to mean anything here.
#[test]
fn an_uncertain_ending_survives_the_store_as_an_absence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("log.sqlite");
    let mut store = Store::open(&path).expect("open");
    let task = a_task(&mut store);
    store
        .append(Event::RunStarted {
            mode: Mode::SinglePlayer,
            version: "0.1.0".into(),
            pid: 4242,
        })
        .expect("run started");
    let attempt = an_attempt(&mut store, task);
    store
        .append(Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Uncertain {
                why: Why::TruncatedAtCap { budget: 8_192 },
            },
        })
        .expect("ending");
    drop(store);

    assert_eq!(Fun::over(&read_all(&path)).honest_failure.failed_runs, 0);
}
