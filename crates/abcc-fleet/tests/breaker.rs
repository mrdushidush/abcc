//! The breaker's arithmetic, against populations small enough to check by hand.
//!
//! Two things are under test and they are different in kind. **F374's rule** —
//! that the value of the next attempt depends on the population and not on a
//! constant — is checked by building two populations with the *same*
//! unconditional rate and showing that one failure moves them by 80 points and 0
//! points respectively. And **the absence exclusion** is checked by building
//! F539's shape: every attempt an absence, and asking whether the report says
//! *nothing was measured* or invents a 0% pass rate.

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::Event;
use abcc_core::outcome::Why;
use abcc_core::seq::{AttemptId, MissionId, TaskId, UnitId};
use abcc_fleet::breaker::{Population, Pulse, Report, Sample};
use abcc_store::Store;

/// A task on the board.
fn task(store: &mut Store, title: &str) -> TaskId {
    let m = store
        .append(Event::MissionCreated {
            title: "breaker".into(),
        })
        .expect("mission");
    let t = store
        .append(Event::TaskCreated {
            mission: MissionId::at(m.seq),
            title: title.into(),
            prompt: "do the thing".into(),
        })
        .expect("task");
    TaskId::at(t.seq)
}

/// One attempt, ended the way the caller says.
///
/// ⚠ It writes `AttemptStarted` and `AttemptEnded` and does not drive the
/// lifecycle, because the breaker is a fold over *events* — that is ADR-0010
/// §4's "reads the event log, not a field on the task", and this test would not
/// notice if it stopped being true any other way.
fn ended(store: &mut Store, task: TaskId, outcome: AttemptOutcome) {
    let started = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: Cause::Fresh,
            checkpoint_from: None,
        })
        .expect("started");
    store
        .append(Event::AttemptEnded {
            task,
            attempt: AttemptId::at(started.seq),
            outcome,
        })
        .expect("ended");
}

fn pass(store: &mut Store, task: TaskId) {
    ended(store, task, AttemptOutcome::Success);
}

fn fail(store: &mut Store, task: TaskId) {
    ended(
        store,
        task,
        AttemptOutcome::Refused {
            rung: "acceptance".into(),
            detail: "the test still fails".into(),
        },
    );
}

/// 🚨 An **absence**, not a failure: the model finished cleanly and said
/// nothing. This is what F539's wedge produced, on every attempt, for 60 s.
fn absence(store: &mut Store, task: TaskId) {
    ended(
        store,
        task,
        AttemptOutcome::Uncertain {
            why: Why::SaidNothing {
                by: "Recon".into(),
                completion_tokens: 47,
                reasoning_tokens: Some(42),
            },
        },
    );
}

fn population(build: impl FnOnce(&mut Store)) -> Population {
    let mut store = Store::in_memory().expect("store");
    build(&mut store);
    Population::of(&store).expect("population")
}

/// A rate as whole percentage points.
///
/// ⚠ It rounds to an integer rather than comparing floats, so every assertion
/// below is exact — the numbers here are all hand-checkable eighths and fifths,
/// and a test that needed a tolerance would be a test whose arithmetic nobody
/// had done.
#[allow(clippy::cast_possible_truncation)]
fn pct(p: Option<f64>) -> i64 {
    (p.expect("a rate") * 100.0).round() as i64
}

/// The same, for an `Option<f64>` share that may legitimately be absent.
#[allow(clippy::cast_possible_truncation)]
fn share(p: Option<f64>) -> Option<i64> {
    p.map(|p| (p * 100.0).round() as i64)
}

// ---------------------------------------------------------------------------
// F374 — the update rule, and why it cannot be a number
// ---------------------------------------------------------------------------

/// 🚨🚨 **The whole finding, in one test.**
///
/// Two populations with the **same 80% unconditional rate**. One is bimodal —
/// four tasks that always pass and one that never does — and one failure takes
/// it to 0%. The other is flat, every task at 80%, and the same failure moves it
/// by nothing at all.
///
/// That 80-point spread between two populations that look identical from their
/// headline rate is why ADR-0010 §4 says **ship the update rule, not the
/// number**, and why a constant *retry twice then escalate* is right on one
/// corpus and wrong on the other. W4 measured the same shape on real corpora:
/// Q56 88.2% → 43.6%, U100 88.8% → 81.9%.
#[test]
fn one_failure_is_worth_80_points_on_a_bimodal_population_and_0_on_a_flat_one() {
    // Bimodal: 4 tasks at p = 1.0, 1 task at p = 0.
    let bimodal = population(|store| {
        for i in 0..4 {
            let t = task(store, &format!("always {i}"));
            pass(store, t);
        }
        let never = task(store, "never");
        fail(store, never);
    });

    // Flat: 5 tasks, each 4 passes and 1 failure, so every p_t is 0.8.
    let flat = population(|store| {
        for i in 0..5 {
            let t = task(store, &format!("middling {i}"));
            for _ in 0..4 {
                pass(store, t);
            }
            fail(store, t);
        }
    });

    // The headline number cannot tell them apart.
    assert_eq!(pct(bimodal.unconditional()), 80);
    assert_eq!(pct(flat.unconditional()), 80);

    // One failure can.
    assert_eq!(pct(bimodal.pass_after(1)), 0);
    assert_eq!(pct(flat.pass_after(1)), 80);

    // And the flat population stays flat however many failures are behind it,
    // because a failure carries no information about which task you are on when
    // every task is the same.
    for k in 0..5 {
        assert_eq!(pct(flat.pass_after(k)), 80, "k = {k}");
    }
}

/// The bimodal population is the one where a third attempt buys little, and the
/// flat one is where it is worth pricing (ADR-0010 §3). The share is reported
/// rather than turned into a rule here.
#[test]
fn bimodality_is_measured_rather_than_assumed() {
    let bimodal = population(|store| {
        let always = task(store, "always");
        pass(store, always);
        let never = task(store, "never");
        fail(store, never);
    });
    assert_eq!(share(bimodal.bimodal_share()), Some(100));

    let flat = population(|store| {
        let t = task(store, "middling");
        pass(store, t);
        fail(store, t);
    });
    assert_eq!(share(flat.bimodal_share()), Some(0));

    assert_eq!(share(Population::default().bimodal_share()), None);
}

/// A population that has never seen `k` failures in a row has nothing to say
/// about them, and says so.
///
/// ⚠ `None` rather than `0.0`. A zero here would be a number where there is no
/// measurement, which is the failure this codebase is organised against.
#[test]
fn a_population_that_never_fails_declines_to_price_a_failure() {
    let perfect = population(|store| {
        for i in 0..3 {
            let t = task(store, &format!("always {i}"));
            pass(store, t);
        }
    });
    assert_eq!(pct(perfect.unconditional()), 100);
    assert_eq!(
        perfect.pass_after(1),
        None,
        "a population of certainties priced an event it has never seen"
    );
}

// ---------------------------------------------------------------------------
// 🚨 F539 — the absence exclusion, which is the difference between measuring
// the work and measuring the instrument
// ---------------------------------------------------------------------------

/// 🚨🚨 **The wedge, as the breaker would see it.**
///
/// Every attempt an absence — which is what F539 produced, with `/v1/models`
/// answering normally throughout. Counted as failures these would give a
/// confident **0% pass rate over a large sample**, and the breaker would report
/// *these tasks are impossible* about a server that was not answering at all.
///
/// So they are counted apart, the rate is `None`, and the standing sentence
/// names both the absences and the silent pulse.
#[test]
fn a_wedged_server_produces_no_rate_rather_than_a_rate_of_zero() {
    let wedged = population(|store| {
        for i in 0..8 {
            let t = task(store, &format!("task {i}"));
            absence(store, t);
        }
    });

    assert_eq!(wedged.absences(), 8);
    assert_eq!(wedged.measured_attempts(), 0);
    assert!(
        wedged.measured_tasks().is_empty(),
        "an absence became a measurement"
    );
    assert_eq!(
        wedged.pass_after(0),
        None,
        "eight absences were priced as eight failures"
    );

    let report = Report::new(
        Pulse::Silent {
            after_ms: 60_000,
            detail: "HTTP 200 with no content in the choice".into(),
        },
        wedged,
    );
    let said = report.standing();
    assert!(said.contains("no population and no rate"), "{said}");
    assert!(
        said.contains('8'),
        "the absences are not counted out: {said}"
    );
    assert!(
        said.contains("F539"),
        "the report does not name the shape it is looking at: {said}"
    );
}

/// An absence beside real measurements neither helps nor hurts the rate, and is
/// still printed.
#[test]
fn an_absence_beside_measurements_stays_out_of_the_rate() {
    let mixed = population(|store| {
        let t = task(store, "one");
        pass(store, t);
        absence(store, t);
        absence(store, t);
        absence(store, t);
    });

    let records = mixed.measured_tasks();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].passed, 1);
    assert_eq!(records[0].failed, 0);
    assert_eq!(records[0].absent, 3);
    assert_eq!(records[0].measured(), 1);
    assert_eq!(pct(mixed.unconditional()), 100);

    let said = Report::new(Pulse::NotTaken, mixed).standing();
    assert!(said.contains("NOT in the rate"), "{said}");
}

// ---------------------------------------------------------------------------
// The report says how much it is worth
// ---------------------------------------------------------------------------

/// ⚠ F544's lesson, generalised: the sample count is printed before anything is
/// read off the table, and a handful of tasks is called arithmetic rather than
/// an estimate.
#[test]
fn a_small_population_is_called_arithmetic_and_not_an_estimate() {
    let small = population(|store| {
        let t = task(store, "one");
        pass(store, t);
        fail(store, t);
    });
    let said = Report::new(Pulse::NotTaken, small).standing();
    assert!(said.contains("arithmetic, not an estimate"), "{said}");

    let larger = population(|store| {
        for i in 0..6 {
            let t = task(store, &format!("task {i}"));
            pass(store, t);
            fail(store, t);
        }
    });
    let said = Report::new(Pulse::NotTaken, larger).standing();
    assert!(said.contains("6 measured tasks over 12 attempts"), "{said}");
    assert!(!said.contains("arithmetic"), "{said}");
}

/// 🚨 A pulse that was not taken must never render as one that answered.
#[test]
fn a_pulse_that_was_not_taken_is_not_a_pulse_that_answered() {
    assert!(!Pulse::NotTaken.answered());
    assert!(
        !Pulse::Silent {
            after_ms: 60_000,
            detail: "nothing".into()
        }
        .answered()
    );
    assert!(
        !Pulse::Unreachable {
            after_ms: 10,
            detail: "refused".into()
        }
        .answered()
    );
    assert!(
        Pulse::Answered {
            elapsed_ms: 412,
            tokens: 1,
            sample: Sample::Reasoning("Thinking".into())
        }
        .answered()
    );

    // The rendering keeps them apart too: the console is where an operator
    // reads this, and three states that print the same are one state.
    let shown: Vec<String> = [
        Pulse::NotTaken,
        Pulse::Silent {
            after_ms: 60_000,
            detail: "nothing".into(),
        },
        Pulse::Unreachable {
            after_ms: 10,
            detail: "refused".into(),
        },
        Pulse::Answered {
            elapsed_ms: 412,
            tokens: 1,
            sample: Sample::Reasoning("Thinking".into()),
        },
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let mut unique = shown.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), shown.len(), "{shown:?}");
}

/// The rows are the rule, evaluated over whatever the log holds — not a table
/// baked in at compile time.
#[test]
fn the_report_rows_are_the_rule_over_this_logs_own_population() {
    let bimodal = population(|store| {
        for i in 0..4 {
            let t = task(store, &format!("always {i}"));
            pass(store, t);
        }
        let never = task(store, "never");
        fail(store, never);
    });
    let report = Report::new(Pulse::NotTaken, bimodal);
    let rows = report.rows();

    assert_eq!(rows.len(), 5, "k = 0..=4");
    assert_eq!(rows[0].0, 0);
    assert_eq!(pct(rows[0].1), 80);
    // Every row after the first is 0%: on this population a single failure
    // already says which task you are on.
    for (k, p) in &rows[1..] {
        assert_eq!(pct(*p), 0, "k = {k}");
    }
}
