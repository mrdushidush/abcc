//! The control point, including the latency ADR-0006 says this design owes
//! itself.
//!
//! The ADR names two tests as *not optional*. The second is here: **pin the
//! cancel latency**, because F200's 3–14 ms is the number the operator's abort
//! button inherits. (The first — pin the per-read timeout semantic — belongs to
//! the HTTP provider and arrives with it.)

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use abcc_core::event::Control;
use abcc_engine::control::{ControlPoint, Disposition, Keep, Urgency, urgency};

fn stop_of(d: Disposition) -> abcc_engine::Stop {
    match d {
        Disposition::Stop(s) => s,
        Disposition::Carry => panic!("expected a stop, got Carry"),
    }
}

// ---------------------------------------------------------------------------
// The two urgencies
// ---------------------------------------------------------------------------

/// `Pause` waits for the boundary: the work in flight completes, which is what
/// distinguishes it from `Halt` and is the only thing that does.
#[test]
fn pause_waits_for_a_step_boundary_and_halt_does_not() {
    let (mut point, handle) = ControlPoint::new();
    handle.request(Control::Pause).expect("worker is alive");
    assert!(
        !point.interrupted(),
        "a pause must not interrupt a stream in flight"
    );
    assert_eq!(stop_of(point.check()).keep, Keep::AtCheckpoint);

    let (mut point, handle) = ControlPoint::new();
    handle.request(Control::Halt).expect("worker is alive");
    assert!(point.interrupted(), "a halt is seen between deltas");
    assert_eq!(stop_of(point.check()).keep, Keep::AtCheckpoint);
}

/// 🚨 Pause and Halt keep the *same* thing. The operator chose when, not what,
/// and inventing a second disposition would be inventing a distinction the verbs
/// do not have.
#[test]
fn pause_and_halt_differ_only_in_urgency() {
    let (mut paused, ph) = ControlPoint::new();
    let (mut halted, hh) = ControlPoint::new();
    ph.request(Control::Pause).unwrap();
    hh.request(Control::Halt).unwrap();
    assert_eq!(stop_of(paused.check()).keep, stop_of(halted.check()).keep);
    assert_ne!(urgency(&Control::Pause), urgency(&Control::Halt));
    assert_eq!(urgency(&Control::Halt), Urgency::Now);
}

/// Every urgent verb is seen between deltas, and each keeps what its ADR says.
#[test]
fn the_urgent_verbs_are_seen_between_deltas() {
    let cases = [
        (Control::Halt, Keep::AtCheckpoint),
        (Control::Kill, Keep::Nothing),
        (
            Control::Redirect {
                prompt: "narrow it to the parser".to_owned(),
            },
            Keep::AndFork {
                prompt: "narrow it to the parser".to_owned(),
            },
        ),
    ];
    for (control, expected) in cases {
        let (mut point, handle) = ControlPoint::new();
        handle.request(control.clone()).unwrap();
        assert!(point.interrupted(), "{control:?} must interrupt");
        let stop = stop_of(point.check());
        assert_eq!(stop.keep, expected, "{control:?}");
        assert_eq!(stop.control, control, "the verb is carried verbatim");
    }
}

/// `Resume` belongs to a task in `Holding`, which by definition has no worker.
#[test]
fn resume_is_not_a_stop() {
    let (mut point, handle) = ControlPoint::new();
    handle.request(Control::Resume).unwrap();
    assert!(!point.interrupted());
    assert_eq!(point.check(), Disposition::Carry);
    assert!(point.latched().is_none());
}

/// The first stop wins and stays won: a second verb arriving while the worker
/// unwinds must not change what it is unwinding into.
#[test]
fn the_first_stop_latches() {
    let (mut point, handle) = ControlPoint::new();
    handle.request(Control::Kill).unwrap();
    handle.request(Control::Pause).unwrap();
    let first = stop_of(point.check());
    assert_eq!(first.keep, Keep::Nothing);

    handle.request(Control::Halt).unwrap();
    assert_eq!(
        stop_of(point.check()).keep,
        Keep::Nothing,
        "a later verb must not re-aim a stop already in progress"
    );
}

/// An attempt that ended before the verb arrived is the ordinary race, not a
/// fault — which is why the durable `ControlRequested` row is the record and this
/// channel is only the poke.
#[test]
fn a_verb_for_a_finished_attempt_is_refused_rather_than_lost() {
    let (point, handle) = ControlPoint::new();
    drop(point);
    assert!(handle.request(Control::Kill).is_err());
}

// ---------------------------------------------------------------------------
// The latency ADR-0006 owes itself
// ---------------------------------------------------------------------------

/// 🚨 **Pin the cancel latency.** F200 measured a live generation stopped at 4 ms
/// and 14 ms by a flag flip and a dropped response; the mechanism under that
/// number is this one, sampled between deltas.
///
/// What is pinned here is the **sampling** half — how long a verb waits for the
/// worker to look — with a delta cadence of 15 ms standing in for an SSE line at
/// measured decode rates. The socket-close half belongs to the provider and is
/// pinned with it.
///
/// The bound is deliberately loose against the 15 ms cadence: this asserts the
/// mechanism is a sample and not a poll of some other period, and a tight bound
/// on a shared box would be measuring the scheduler.
#[test]
fn an_urgent_verb_is_seen_within_a_delta_or_two() {
    const CADENCE: Duration = Duration::from_millis(15);
    const BOUND: Duration = Duration::from_millis(250);

    let (point, handle) = ControlPoint::new();
    let (tx, rx) = mpsc::channel();

    let worker = thread::spawn(move || {
        // Stand in for a turn loop reading one SSE line at a time.
        for _ in 0..400 {
            if point.interrupted() {
                let _ = tx.send(Instant::now());
                return true;
            }
            thread::sleep(CADENCE);
        }
        let _ = tx.send(Instant::now());
        false
    });

    thread::sleep(Duration::from_millis(50));
    let requested = Instant::now();
    handle.request(Control::Halt).expect("worker is alive");

    let observed = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker replied");
    let saw_it = worker.join().expect("worker did not panic");

    assert!(saw_it, "the worker ran to the end without seeing the halt");
    let latency = observed.saturating_duration_since(requested);
    assert!(
        latency < BOUND,
        "a halt took {latency:?} to be seen at a {CADENCE:?} cadence"
    );
}

/// The flag is set *after* the verb is on the channel, so a worker woken by the
/// flag always finds the verb waiting. The reverse order would produce a wake
/// with nothing to read, which reads as a spurious interrupt rather than as the
/// verb it was.
#[test]
fn the_verb_is_on_the_channel_before_the_flag_is_set() {
    let (mut point, handle) = ControlPoint::new();
    handle.request(Control::Kill).unwrap();
    assert!(point.interrupted());
    assert_eq!(
        stop_of(point.check()).control,
        Control::Kill,
        "the flag was observable but the verb was not"
    );
}
