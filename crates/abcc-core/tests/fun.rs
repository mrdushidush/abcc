//! ADR-0012 §5's six queries, and the two things about them that are easy to
//! get wrong in a way no assertion would notice.
//!
//! The queries themselves are arithmetic. What these tests are really for is the
//! pair of claims underneath them:
//!
//! 1. **A query with no instrument does not report a zero.** Three of the six
//!    ask what the operator did at the console, and nothing writes that down. A
//!    zero would read as a perfect score.
//! 2. **A run's span ends where the run does, not where the next one starts.**
//!    There is no `RunEnded` on the log, so an `abcc accept` typed an hour later
//!    is, to a naive fold, an hour of dead air inside the previous run.
//!
//! And the limit that is not a bug but must not be forgotten: the forty-minute
//! hang the dead-air bar exists for is **invisible to a fold**, because a gap
//! needs an event on both sides.

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::{Control, Event, Logged};
use abcc_core::fun::{Answer, Fun, Missing, SILENCE_BAR_MS, Spread, Verdict, in_flight};
use abcc_core::outcome::Why;
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, MissionId, Seq, TaskId, UnitId};
use abcc_core::task::{Command, TaskState};

/// A log under construction, with a wall clock the test moves on purpose.
struct Log {
    events: Vec<Logged>,
    at_ms: i64,
}

impl Log {
    fn new() -> Log {
        Log {
            events: Vec::new(),
            at_ms: 1_700_000_000_000,
        }
    }

    /// Append `event`, `ms` after the previous one.
    fn after(&mut self, ms: i64, event: Event) -> Seq {
        self.at_ms += ms;
        let seq = Seq::new(i64::try_from(self.events.len()).expect("test log fits an i64") + 1);
        self.events.push(Logged {
            seq,
            at_ms: self.at_ms,
            event,
        });
        seq
    }

    fn now(&self) -> i64 {
        self.at_ms
    }
}

fn run_started() -> Event {
    Event::RunStarted {
        mode: Mode::SinglePlayer,
        version: "0.1.0".to_owned(),
        pid: 4242,
    }
}

fn liveness(attempt: AttemptId) -> Event {
    Event::LivenessMark {
        attempt,
        note: "streaming".to_owned(),
    }
}

/// The `TaskTransitioned` an `abcc accept` writes — the event that makes a naive
/// span run on past the end of the run.
fn accepted(task: TaskId, unit: UnitId, since: Seq) -> Event {
    Event::TaskTransitioned {
        task,
        command: Command::Deploy { unit },
        from: TaskState::Queued,
        to: TaskState::Deployed { unit, since },
    }
}

/// One run: `RunStarted`, an attempt, two liveness marks a second apart, and an
/// ending. Nothing here is over any bar.
fn a_quiet_run() -> Log {
    let mut log = Log::new();
    log.after(0, run_started());
    let task = TaskId::at(log.after(
        5,
        Event::TaskCreated {
            mission: MissionId::at(Seq::new(1)),
            title: "a task".to_owned(),
            prompt: "do the thing".to_owned(),
        },
    ));
    let attempt = AttemptId::at(log.after(
        10,
        Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: Cause::Fresh,
            checkpoint_from: None,
        },
    ));
    log.after(1_000, liveness(attempt));
    log.after(1_000, liveness(attempt));
    log.after(
        50,
        Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Success,
        },
    );
    log
}

// ---------------------------------------------------------------------------
// 1. the three that have no instrument
// ---------------------------------------------------------------------------

/// 🚨 **The load-bearing test.** Three of the six queries have nothing to fold,
/// and the failure this guards against is not a crash — it is a report that says
/// `0` and reads as a perfect score.
#[test]
fn the_three_without_an_instrument_are_never_a_zero() {
    let log = a_quiet_run();
    let fun = Fun::over(&log.events);

    assert_eq!(
        fun.legibility,
        Answer::NoInstrument(Missing::ScreenSwitch),
        "screen switches: 0 would read as perfect legibility"
    );
    assert_eq!(
        fun.personality,
        Answer::NoInstrument(Missing::SettingChange),
        "setting changes: 0 would read as nobody needing the off switch"
    );
    assert_eq!(
        fun.honest_failure.replayed,
        Answer::NoInstrument(Missing::ReplayCursor),
        "replays: 0 would read as nobody caring about failures"
    );

    // The absence names what would fix it, so the report can say so out loud.
    assert_eq!(Missing::ScreenSwitch.needs(), "Event::ScreenSwitched");
    assert_eq!(Missing::SettingChange.needs(), "Event::SettingChanged");
    assert_eq!(Missing::ReplayCursor.needs(), "Event::ReplayCursorMoved");
    assert!(
        Missing::ScreenSwitch.why().contains("reader"),
        "the reason is the architecture, and the report should say which"
    );
}

/// A query with no instrument is `Unknown` and never `Held`. `Unknown` is not a
/// soft pass.
#[test]
fn no_instrument_is_unknown_and_not_a_pass() {
    let fun = Fun::over(&a_quiet_run().events);
    assert_eq!(fun.honest_failure.verdict(), Verdict::Unknown);
}

// ---------------------------------------------------------------------------
// 2. a run's span
// ---------------------------------------------------------------------------

/// 🚨 The span test. An operator accepting the work an hour later is not an hour
/// of dead air inside the run — but it is exactly that to a fold that ends a
/// span at the next `RunStarted`.
#[test]
fn an_operator_command_an_hour_later_is_not_dead_air() {
    let mut log = a_quiet_run();
    let task = TaskId::at(Seq::new(2));
    // An hour of the operator being asleep, then `abcc accept`.
    log.after(3_600_000, accepted(task, UnitId(0), Seq::new(2)));

    let fun = Fun::over(&log.events);
    assert_eq!(
        fun.dead_air.over_bar, 0,
        "the hour belongs to the operator, not to the run"
    );
    assert_eq!(fun.dead_air.verdict(), Verdict::Held);
    assert_eq!(
        fun.loose, 1,
        "and the event is still counted — as belonging to no run"
    );
    // Without the span rule this is 3_600_000. Assert the number, not just the
    // verdict: a max that quietly included the hour would still be `Held` if the
    // bar moved.
    let spread = fun.dead_air.spread.expect("gaps");
    assert!(
        spread.max_ms < 3_600_000,
        "the hour leaked into the run's gaps: max was {} ms",
        spread.max_ms
    );
    assert_eq!(
        spread.max_ms, 1_000,
        "the real worst gap is a liveness mark"
    );
}

/// The discriminator the span rule rests on. `TaskTransitioned` is written from
/// both sides, so it cannot hold a span open; a `LivenessMark` can only come
/// from a run in flight.
#[test]
fn in_flight_is_the_discriminator_the_span_needs() {
    assert!(in_flight(&liveness(AttemptId::at(Seq::new(3)))));
    assert!(in_flight(&run_started()));
    assert!(
        !in_flight(&accepted(TaskId::at(Seq::new(2)), UnitId(0), Seq::new(2))),
        "`abcc accept` writes one of these hours after the run"
    );
    assert!(
        !in_flight(&Event::Note {
            text: "abcc check".to_owned()
        }),
        "a note is written from both sides, so it may not hold a span open"
    );
}

/// Events before the first run belong to no run and produce no gaps at all —
/// `abcc task` on Monday and `abcc run` on Friday is not four days of dead air.
#[test]
fn events_before_the_first_run_belong_to_no_run() {
    let mut log = Log::new();
    log.after(
        0,
        Event::MissionCreated {
            title: "a mission".to_owned(),
        },
    );
    log.after(4 * 86_400_000, run_started());
    let attempt = AttemptId::at(Seq::new(2));
    log.after(500, liveness(attempt));

    let fun = Fun::over(&log.events);
    assert_eq!(fun.runs, 1);
    assert_eq!(fun.loose, 1, "the mission belongs to no run");
    assert_eq!(fun.dead_air.over_bar, 0, "four days is not dead air");
    assert_eq!(fun.dead_air.spread.expect("one gap").max_ms, 500);
}

// ---------------------------------------------------------------------------
// 3. dead air, and the half a fold cannot see
// ---------------------------------------------------------------------------

#[test]
fn a_gap_over_ten_seconds_breaks_the_bar_and_names_itself() {
    let mut log = Log::new();
    log.after(0, run_started());
    let attempt = AttemptId::at(Seq::new(1));
    let quiet = log.after(200, liveness(attempt));
    let after_silence = log.after(SILENCE_BAR_MS + 1, liveness(attempt));

    let fun = Fun::over(&log.events);
    assert_eq!(fun.dead_air.verdict(), Verdict::Broken);
    assert_eq!(fun.dead_air.over_bar, 1);
    let worst = fun.dead_air.worst.expect("a worst gap");
    assert_eq!(worst.ms, SILENCE_BAR_MS + 1);
    assert_eq!(
        (worst.after, worst.before),
        (quiet, after_silence),
        "a verdict you cannot go and look at is a verdict you have to believe"
    );
}

/// 🚨 **The forty-minute hang is invisible to the fold, and that is not a bug in
/// the fold.** A gap needs an event on both sides and a hung run writes no
/// second one — so the query that catches it is the one that takes `now`.
#[test]
fn the_hang_the_bar_exists_for_needs_the_wall_clock() {
    let mut log = Log::new();
    log.after(0, run_started());
    let attempt = AttemptId::at(Seq::new(1));
    log.after(200, liveness(attempt));
    // ...and then the process wedges. Nothing more is ever written.

    let fun = Fun::over(&log.events);
    assert_eq!(
        fun.dead_air.verdict(),
        Verdict::Held,
        "every gap that closed was inside the bar — and there is only one"
    );
    assert_eq!(fun.dead_air.over_bar, 0);

    // Forty minutes later, W5's actual measurement.
    let forty_minutes = 40 * 60 * 1_000;
    let open = Fun::open_silence(&log.events, log.now() + forty_minutes).expect("a last event");
    assert_eq!(open, forty_minutes);
    assert!(
        open > SILENCE_BAR_MS,
        "the one silence that matters is only visible against a clock"
    );
}

#[test]
fn an_empty_log_has_nothing_to_judge() {
    let fun = Fun::over(&[]);
    assert_eq!(fun.runs, 0);
    assert_eq!(fun.dead_air.verdict(), Verdict::Unknown);
    assert_eq!(fun.speed.verdict(), Verdict::Unknown);
    assert_eq!(fun.agency.verdict(), Verdict::Unknown);
    assert_eq!(Fun::open_silence(&[], 0), None);
}

// ---------------------------------------------------------------------------
// 4. speed, and the command nobody answered
// ---------------------------------------------------------------------------

#[test]
fn a_control_verb_is_timed_to_the_next_thing_on_the_log() {
    let mut log = Log::new();
    log.after(0, run_started());
    let task = TaskId::at(Seq::new(1));
    let attempt = AttemptId::at(Seq::new(1));
    log.after(
        100,
        Event::ControlRequested {
            task,
            control: Control::Halt,
        },
    );
    log.after(120, liveness(attempt));

    let fun = Fun::over(&log.events);
    assert_eq!(fun.speed.verdict(), Verdict::Held);
    assert_eq!(fun.speed.spread.expect("one answer").n, 1);
    assert_eq!(fun.speed.slowest.expect("the slowest").ms, 120);
    assert_eq!(fun.speed.over_bar, 0);
    assert_eq!(fun.speed.unanswered, 0);
}

/// 🚨 The live sortie of 2026-09-04 ended exactly here: a `ControlRequested` for
/// a task that was never admitted. That is an absence, not a slow answer, and
/// folding it in with the latencies would put a missing measurement into a
/// number about speed.
#[test]
fn a_command_the_run_never_answered_is_an_absence_and_not_a_slow_answer() {
    let mut log = Log::new();
    log.after(0, run_started());
    let task = TaskId::at(Seq::new(1));
    log.after(
        100,
        Event::ControlRequested {
            task,
            control: Control::Halt,
        },
    );

    let fun = Fun::over(&log.events);
    assert_eq!(fun.speed.unanswered, 1);
    assert!(
        fun.speed.spread.is_none(),
        "an unanswered command is not a sample"
    );
    assert_eq!(
        fun.speed.verdict(),
        Verdict::Unknown,
        "no answer was measured, so nothing held and nothing broke"
    );
    // 🚨 And the agency query still counts it: the operator did reach for it.
    assert_eq!(fun.agency.events, 1);
}

#[test]
fn an_answer_over_a_second_breaks_the_speed_bar() {
    let mut log = Log::new();
    log.after(0, run_started());
    let task = TaskId::at(Seq::new(1));
    log.after(
        100,
        Event::ControlRequested {
            task,
            control: Control::Pause,
        },
    );
    log.after(1_001, liveness(AttemptId::at(Seq::new(1))));

    let fun = Fun::over(&log.events);
    assert_eq!(fun.speed.over_bar, 1);
    assert_eq!(fun.speed.verdict(), Verdict::Broken);
}

// ---------------------------------------------------------------------------
// 5. agency — the query that could not return non-zero before 2026-09-04
// ---------------------------------------------------------------------------

#[test]
fn a_run_nobody_touched_says_the_control_bar_is_decoration() {
    let fun = Fun::over(&a_quiet_run().events);
    assert_eq!(fun.agency.runs, 1);
    assert_eq!(fun.agency.events, 0);
    assert_eq!(fun.agency.runs_with_control, 0);
    assert_eq!(fun.agency.verdict(), Verdict::Broken);
}

/// Two runs, one of them touched. The count is per run and not per log.
#[test]
fn control_events_are_counted_per_run() {
    let mut log = a_quiet_run();
    log.after(1_000, run_started());
    let task = TaskId::at(Seq::new(2));
    log.after(
        50,
        Event::ControlRequested {
            task,
            control: Control::Kill,
        },
    );
    log.after(
        50,
        Event::ControlRequested {
            task,
            control: Control::Halt,
        },
    );
    log.after(20, liveness(AttemptId::at(Seq::new(3))));

    let fun = Fun::over(&log.events);
    assert_eq!(fun.runs, 2);
    assert_eq!(fun.agency.events, 2, "both verbs");
    assert_eq!(fun.agency.runs_with_control, 1, "but only one run");
    assert_eq!(fun.agency.verdict(), Verdict::Held);
}

// ---------------------------------------------------------------------------
// 6. honest failure — the denominator, and what does not go in it
// ---------------------------------------------------------------------------

/// 🚨 `Uncertain` is an absence and not a score. Counting it as a failed run
/// would put the model stopping at the token cap into a number about runs that
/// were judged and found wanting.
#[test]
fn an_uncertain_ending_is_not_a_failed_run() {
    let mut log = Log::new();
    log.after(0, run_started());
    let task = TaskId::at(Seq::new(1));
    let attempt = AttemptId::at(Seq::new(1));
    log.after(
        10,
        Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Uncertain {
                why: Why::TruncatedAtCap { budget: 8_192 },
            },
        },
    );

    let fun = Fun::over(&log.events);
    assert_eq!(fun.honest_failure.failed_runs, 0);
}

#[test]
fn the_three_endings_decided_against_an_attempt_are_failed_runs() {
    for outcome in [
        AttemptOutcome::SoftFailure {
            why: Why::Timeout { after_ms: 500 },
        },
        AttemptOutcome::HardFailure {
            why: Why::EngineError {
                detail: "no".to_owned(),
            },
        },
        AttemptOutcome::Refused {
            rung: "cargo test".to_owned(),
            detail: "1 failed".to_owned(),
        },
    ] {
        let mut log = Log::new();
        log.after(0, run_started());
        log.after(
            10,
            Event::AttemptEnded {
                task: TaskId::at(Seq::new(1)),
                attempt: AttemptId::at(Seq::new(1)),
                outcome: outcome.clone(),
            },
        );
        let fun = Fun::over(&log.events);
        assert_eq!(
            fun.honest_failure.failed_runs, 1,
            "{outcome:?} is a run that failed"
        );
    }
}

/// A run with several failed attempts is one failed run, not several.
#[test]
fn a_failed_run_is_counted_once_however_many_attempts_failed() {
    let mut log = Log::new();
    log.after(0, run_started());
    for _ in 0..3 {
        log.after(
            10,
            Event::AttemptEnded {
                task: TaskId::at(Seq::new(1)),
                attempt: AttemptId::at(Seq::new(1)),
                outcome: AttemptOutcome::SoftFailure {
                    why: Why::Timeout { after_ms: 500 },
                },
            },
        );
    }
    assert_eq!(Fun::over(&log.events).honest_failure.failed_runs, 1);
}

// ---------------------------------------------------------------------------
// the arithmetic
// ---------------------------------------------------------------------------

/// Nearest-rank, so every number reported is a gap that actually happened.
#[test]
fn percentiles_are_nearest_rank() {
    let s = Spread::of(vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100]).expect("a sample");
    assert_eq!(s.n, 10);
    assert_eq!(s.p50_ms, 50, "the 5th of 10, and a value from the sample");
    assert_eq!(s.p95_ms, 100);
    assert_eq!(s.max_ms, 100);

    let one = Spread::of(vec![7]).expect("a sample of one");
    assert_eq!((one.p50_ms, one.p95_ms, one.max_ms), (7, 7, 7));
    assert_eq!(Spread::of(Vec::new()), None, "no sample is not a zero");
}

/// The fold does not depend on the sample arriving sorted.
#[test]
fn a_spread_sorts_what_it_is_given() {
    let s = Spread::of(vec![90, 10, 50, 30, 70]).expect("a sample");
    assert_eq!((s.p50_ms, s.max_ms), (50, 90));
}

/// Every event is accounted for: in a span or loose, never both and never
/// neither.
#[test]
fn every_event_is_either_in_a_run_or_loose() {
    let mut log = a_quiet_run();
    log.after(
        3_600_000,
        accepted(TaskId::at(Seq::new(2)), UnitId(0), Seq::new(2)),
    );
    log.after(1_000, run_started());
    log.after(10, liveness(AttemptId::at(Seq::new(8))));

    let fun = Fun::over(&log.events);
    assert_eq!(fun.events, log.events.len());
    assert_eq!(fun.runs, 2);
    assert_eq!(fun.loose, 1);
}
