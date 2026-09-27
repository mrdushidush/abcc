//! The after-action fold, and the four claims underneath it that no amount of
//! arithmetic would notice going wrong.
//!
//! 1. **A state groups by name and never by value.** Every lifecycle variant but
//!    `Queued` carries a `since`, so two tasks in the same state are unequal —
//!    a table keyed by the value is one row per task, and it looks exactly like
//!    a table that works until a second task lands in the same state.
//! 2. **An ending groups by the `Why`'s variant and never by its `Display`.**
//!    The payloads carry budgets and detail strings, so a histogram over the
//!    rendered sentence counts to one forever.
//! 3. **A phase's name is the entry before its ending.** `PhaseEnded` does not
//!    repeat the phase, deliberately, so a fold that does not pair them has no
//!    phase at all — and every other assertion about a phase still passes.
//! 4. **A silence is named by what broke it.** A `LivenessMark` closing a gap is
//!    the run saying *still here*; anything else closing one means nothing was
//!    watching for that whole stretch.
//!
//! And the limit that is a property rather than a bug, asserted here so it stays
//! known: **a gap needs an event on both sides**, so the hang the bar exists for
//! writes no second event and the fold cannot see it.

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::{CallShape, Composition, Event, Finish, Logged, TraceSignal, Usage};
use abcc_core::outcome::Why;
use abcc_core::redact::Secrets;
use abcc_core::replay::{Replay, ending};
use abcc_core::run::{AttemptPhase, MissionPhase};
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

    /// A mission, a task, and the transition that engages it. Returns the task
    /// and the attempt started on it.
    fn a_task_under_attempt(&mut self, title: &str) -> (TaskId, AttemptId) {
        let m = self.after(
            0,
            Event::MissionCreated {
                title: "m".to_owned(),
            },
        );
        let t = self.after(
            0,
            Event::TaskCreated {
                mission: MissionId::at(m),
                title: title.to_owned(),
                prompt: "do the thing".to_owned(),
            },
        );
        let task = TaskId::at(t);
        let a = self.after(
            0,
            Event::AttemptStarted {
                task,
                unit: UnitId(0),
                cause: Cause::Fresh,
                checkpoint_from: None,
            },
        );
        (task, AttemptId::at(a))
    }

    /// End `attempt`, and land `task` in `state`.
    fn end(&mut self, task: TaskId, attempt: AttemptId, outcome: AttemptOutcome, to: TaskState) {
        self.after(
            0,
            Event::AttemptEnded {
                task,
                attempt,
                outcome,
            },
        );
        self.after(
            0,
            Event::TaskTransitioned {
                task,
                command: Command::Fail { attempt },
                from: TaskState::Engaged {
                    attempt,
                    since: Seq::new(1),
                },
                to,
            },
        );
    }
}

fn usage(completion: u32, reasoning: Option<u32>) -> Usage {
    Usage {
        prompt_tokens: 100,
        completion_tokens: completion,
        reasoning_tokens: reasoning,
        cached_tokens: None,
    }
}

// ---------------------------------------------------------------------------
// 1. a state groups by name, not by value
// ---------------------------------------------------------------------------

/// 🚨 The positive control for the grouping key. Both tasks reach `Failed`, and
/// the two `Failed` values are **not equal** because each names its own attempt
/// — so a fold keyed by the value produces two rows and this fails.
#[test]
fn two_tasks_in_one_state_are_one_row() {
    let mut log = Log::new();
    for (title, why) in [
        ("first", Why::TruncatedAtCap { budget: 8192 }),
        (
            "second",
            Why::SaidNothing {
                by: "Builders".to_owned(),
                completion_tokens: 5,
                reasoning_tokens: None,
            },
        ),
    ] {
        let (task, attempt) = log.a_task_under_attempt(title);
        log.end(
            task,
            attempt,
            AttemptOutcome::Uncertain { why },
            TaskState::Failed { attempt },
        );
    }

    let rows = Replay::over(&log.events).endings();
    assert_eq!(rows.len(), 1, "two Failed tasks must be one row, not two");
    assert_eq!(rows[0].state.name(), "Failed");
    assert_eq!(rows[0].tasks.len(), 2);
    assert_eq!(rows[0].attempts(), 2);
    assert_eq!(
        rows[0].endings.len(),
        2,
        "and the two endings under it stay distinct"
    );
}

/// 🚨 The other half: one word over several endings is the whole reason the
/// view exists, and `uncertain()` is what says how much of it is *could not
/// tell* rather than *was wrong*.
#[test]
fn one_state_counts_its_uncertain_endings_apart() {
    let mut log = Log::new();
    let cases = [
        AttemptOutcome::Uncertain {
            why: Why::TruncatedAtCap { budget: 8192 },
        },
        AttemptOutcome::Uncertain {
            why: Why::TruncatedAtCap { budget: 4096 },
        },
        AttemptOutcome::HardFailure {
            why: Why::EngineError {
                detail: "HTTP 500".to_owned(),
            },
        },
    ];
    for (i, outcome) in cases.into_iter().enumerate() {
        let (task, attempt) = log.a_task_under_attempt(&format!("t{i}"));
        log.end(task, attempt, outcome, TaskState::Failed { attempt });
    }

    let rows = Replay::over(&log.events).endings();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempts(), 3);
    assert_eq!(
        rows[0].uncertain(),
        2,
        "two of the three are Uncertain, and the third is a real failure"
    );
}

// ---------------------------------------------------------------------------
// 2. an ending groups by the variant, not by the sentence
// ---------------------------------------------------------------------------

/// 🚨 Two truncations at different caps are **one** ending. `Why`'s `Display`
/// writes the budget into the sentence, so grouping on it would count to one
/// twice and the commonest failure in this project's log would vanish into
/// singletons.
#[test]
fn two_truncations_at_different_caps_are_one_ending() {
    let a = AttemptOutcome::Uncertain {
        why: Why::TruncatedAtCap { budget: 8192 },
    };
    let b = AttemptOutcome::Uncertain {
        why: Why::TruncatedAtCap { budget: 4096 },
    };
    assert_eq!(ending(&a), ending(&b));
    assert_eq!(ending(&a), "Uncertain/TruncatedAtCap");

    // 🚨 And the sentence they would otherwise have been grouped by does
    // differ. Without this the assertion above would hold just as well over a
    // `Why` whose `Display` ignored its payload, and would be measuring nothing.
    assert_ne!(
        Why::TruncatedAtCap { budget: 8192 }.to_string(),
        Why::TruncatedAtCap { budget: 4096 }.to_string()
    );
}

/// A refusal is named by its rung, because it carries no `Why` at all — the two
/// definite endings are the two that need none.
#[test]
fn a_refusal_is_named_by_its_rung() {
    let refused = AttemptOutcome::Refused {
        rung: "structural".to_owned(),
        detail: "the tree is unchanged".to_owned(),
    };
    assert_eq!(ending(&refused), "Refused/structural");
    assert_eq!(ending(&AttemptOutcome::Success), "Success");
}

// ---------------------------------------------------------------------------
// 3. a phase's name is the entry before its ending
// ---------------------------------------------------------------------------

/// 🚨 `PhaseEnded` carries no phase. If the fold does not pair it with the last
/// `AttemptPhaseEntered`, every count below still lands and the phase is simply
/// wrong — which is the shape a test that asserts only the numbers cannot see.
#[test]
fn a_phase_ending_takes_the_name_of_the_entry_before_it() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("paired");
    for (phase, by, turns) in [
        (AttemptPhase::Localize, "Recon", 2u32),
        (AttemptPhase::Change, "Builders", 7u32),
    ] {
        log.after(0, Event::AttemptPhaseEntered { attempt, phase });
        log.after(
            0,
            Event::PhaseEnded {
                attempt,
                by: by.to_owned(),
                turns,
                tool_calls: 1,
                denials: 0,
                prompt_tokens: 10,
                completion_tokens: 20,
                reasoning_tokens: Some(5),
                trace: TraceSignal::Closed,
                elapsed_ms: 1_000,
            },
        );
    }

    let replay = Replay::over(&log.events);
    let phases = &replay.tasks[0].attempts[0].phases;
    assert_eq!(phases.len(), 2);
    // The pairing itself: the second ending must land on the second entry.
    assert_eq!(phases[0].phase, AttemptPhase::Localize);
    assert_eq!(phases[0].by.as_deref(), Some("Recon"));
    assert_eq!(phases[0].turns, 2);
    assert_eq!(phases[1].phase, AttemptPhase::Change);
    assert_eq!(phases[1].by.as_deref(), Some("Builders"));
    assert_eq!(phases[1].turns, 7);
}

/// A phase entered and never ended is where the attempt stopped, and it says so
/// rather than being dropped.
#[test]
fn a_phase_that_never_ended_is_where_the_attempt_stopped() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("stopped");
    log.after(
        0,
        Event::AttemptPhaseEntered {
            attempt,
            phase: AttemptPhase::Change,
        },
    );

    let replay = Replay::over(&log.events);
    let a = &replay.tasks[0].attempts[0];
    assert_eq!(a.open_phase, Some(AttemptPhase::Change));
    assert!(a.phases[0].is_open());
    assert!(a.is_open(), "and the attempt itself never ended");
}

// ---------------------------------------------------------------------------
// 4. a silence is named by what broke it
// ---------------------------------------------------------------------------

/// 🚨 The distinction the field exists for. Both gaps are the same length; one
/// is closed by the run saying *still here* and one by the next piece of work
/// simply arriving, and only the second means nothing was watching.
#[test]
fn a_silence_is_named_by_the_event_that_broke_it() {
    let mut marked = Log::new();
    let (_, attempt) = marked.a_task_under_attempt("marked");
    marked.after(
        30_000,
        Event::LivenessMark {
            attempt,
            note: "streaming".to_owned(),
        },
    );
    let q = Replay::over(&marked.events).tasks[0].attempts[0]
        .quiet
        .clone()
        .expect("a gap with events on both sides");
    assert_eq!(q.gap_ms, 30_000);
    assert!(q.was_marked(), "a LivenessMark closed it");

    let mut unmarked = Log::new();
    let (_, attempt) = unmarked.a_task_under_attempt("unmarked");
    unmarked.after(
        30_000,
        Event::AttemptPhaseEntered {
            attempt,
            phase: AttemptPhase::Measure,
        },
    );
    let q = Replay::over(&unmarked.events).tasks[0].attempts[0]
        .quiet
        .clone()
        .expect("a gap with events on both sides");
    assert_eq!(q.gap_ms, 30_000);
    assert!(
        !q.was_marked(),
        "the same silence, and nothing was watching it"
    );
    assert_eq!(q.broken_by, "attempt_phase_entered");
}

/// ⚠ **The limit, asserted so it stays known.** A gap needs an event on both
/// sides: an attempt that hung after its last event has no second side, so the
/// silence the bar exists for produces no `Quiet` at all. Seeing it needs the
/// wall clock, which is not on the log.
#[test]
fn a_hang_after_the_last_event_produces_no_gap() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("hung");
    log.after(
        1_000,
        Event::ModelCallStarted {
            attempt,
            provider: "local".to_owned(),
            model: "m".to_owned(),
            head: "Builders".to_owned(),
            head_digest: String::new(),
            ceiling: String::new(),
            budget: 8192,
            seed: 0,
            temperature: String::new(),
        },
    );
    // ...and then the process hangs. Nothing further is ever written.

    let replay = Replay::over(&log.events);
    let a = &replay.tasks[0].attempts[0];
    assert_eq!(
        a.over_bar, 0,
        "no silence over the bar, because the silence never ended"
    );
    assert_eq!(
        a.quiet.as_ref().map(|q| q.gap_ms),
        Some(1_000),
        "only the gaps that closed are visible, and that is the point"
    );
}

/// The bar is counted, not just the worst gap — a single 12 s stall and four of
/// them are different facts about a run.
#[test]
fn every_silence_over_the_bar_is_counted() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("stalls");
    for _ in 0..3 {
        log.after(
            11_000,
            Event::LivenessMark {
                attempt,
                note: "slow".to_owned(),
            },
        );
    }
    log.after(
        5,
        Event::LivenessMark {
            attempt,
            note: "quick".to_owned(),
        },
    );

    let a = &Replay::over(&log.events).tasks[0].attempts[0];
    assert_eq!(a.over_bar, 3, "three over the bar and one under it");
    assert_eq!(a.marks, 4, "and every mark is counted");
}

// ---------------------------------------------------------------------------
// F506 / F511 — what a turn was made of, and what was thrown away
// ---------------------------------------------------------------------------

/// 🚨 **F506: the log is the only copy.** A turn cut at the cap never runs its
/// calls, so an argument string kept on the log is the only record of what was
/// being written — and a call from a turn that *was* used keeps nothing, which
/// is what makes the first one findable.
#[test]
fn only_a_discarded_call_keeps_its_arguments() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("discarded");
    log.after(
        0,
        Event::ModelCallEnded {
            attempt,
            usage: usage(20, Some(5)),
            finish: Finish::ToolCalls,
            ttfb_ms: 100,
            elapsed_ms: 200,
            composition: Some(Composition {
                text_chars: 0,
                reasoning_chars: 40,
                calls: vec![CallShape {
                    tool: "read_file".to_owned(),
                    argument_chars: 33,
                    arguments: None,
                }],
            }),
        },
    );
    log.after(
        0,
        Event::ModelCallEnded {
            attempt,
            usage: usage(8192, Some(96)),
            finish: Finish::Length {
                content_empty: false,
            },
            ttfb_ms: 100,
            elapsed_ms: 200,
            composition: Some(Composition {
                text_chars: 123,
                reasoning_chars: 344,
                calls: vec![CallShape {
                    tool: "apply_patch".to_owned(),
                    argument_chars: 0,
                    arguments: Some(String::new()),
                }],
            }),
        },
    );

    let a = &Replay::over(&log.events).tasks[0].attempts[0];
    assert_eq!(a.discarded.len(), 1, "the used call keeps nothing");
    assert_eq!(a.discarded[0].tool, "apply_patch");
    assert_eq!(
        a.discarded[0].argument_chars, 0,
        "cut before one argument character arrived (F506)"
    );
}

/// 🚨 **The finding this view was built to make visible.** The composition is
/// what F511 added to answer *where did the completion go*, and on a full-cap
/// truncation it accounts for a rounding error of what the server billed: 467
/// characters — about 117 tokens at this stack's ~4:1 — against 8,192 charged.
///
/// ⚠ This asserts the arithmetic of the fold over that shape, not a claim about
/// why the server said 8,192. Which of the two candidates it is (nothing counted
/// the argument deltas, or the tokens were never streamed) is a live run's
/// question, not a fold's.
#[test]
fn a_full_cap_truncation_accounts_for_almost_none_of_what_was_billed() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("truncated");
    log.after(
        0,
        Event::ModelCallEnded {
            attempt,
            usage: usage(8192, Some(96)),
            finish: Finish::Length {
                content_empty: false,
            },
            ttfb_ms: 100,
            elapsed_ms: 200,
            composition: Some(Composition {
                text_chars: 123,
                reasoning_chars: 344,
                calls: vec![CallShape {
                    tool: "apply_patch".to_owned(),
                    argument_chars: 0,
                    arguments: Some(String::new()),
                }],
            }),
        },
    );

    let a = &Replay::over(&log.events).tasks[0].attempts[0];
    assert_eq!(a.made.counted, 1, "every call reported a composition");
    assert_eq!(a.made.chars(), 467);
    assert_eq!(a.spend.completion_tokens, 8192);
    // ~117 tokens accounted against 8,192 billed.
    assert!(
        a.made.chars() / 4 * 100 / a.spend.completion_tokens < 2,
        "under 2% of the billed completion is accounted for"
    );
    assert_eq!(
        a.spend.finishes,
        vec![("length", 1)],
        "and `length` with content is not `length/empty` — F506 splits on that bit"
    );
}

/// `length/empty` is its own ending, because that bit decides whether the turn's
/// tool calls ran at all.
#[test]
fn a_cut_with_an_empty_payload_is_its_own_finish() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("empty");
    log.after(
        0,
        Event::ModelCallEnded {
            attempt,
            usage: usage(4, None),
            finish: Finish::Length {
                content_empty: true,
            },
            ttfb_ms: 10,
            elapsed_ms: 20,
            composition: None,
        },
    );

    let a = &Replay::over(&log.events).tasks[0].attempts[0];
    assert_eq!(a.spend.finishes, vec![("length/empty", 1)]);
    assert_eq!(
        a.made.counted, 0,
        "no composition reported, which is not a composition of zero"
    );
    assert_eq!(
        a.spend.reasoning_tokens, None,
        "and nobody counted reasoning, which is not zero reasoning"
    );
}

// ---------------------------------------------------------------------------
// reading from the middle
// ---------------------------------------------------------------------------

/// ⚠ A page read from the middle of the log is a normal thing to be handed. An
/// event naming an attempt this fold never saw start is **dropped**, not
/// invented — a synthesised attempt would be a row nothing on disk backs.
#[test]
fn an_event_for_an_unseen_attempt_is_dropped_rather_than_invented() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("real");
    let stranger = AttemptId::at(Seq::new(9_999));
    log.after(
        0,
        Event::PhaseNudged {
            attempt: stranger,
            by: "Builders".to_owned(),
            left: 1,
        },
    );
    log.after(
        0,
        Event::PhaseNudged {
            attempt,
            by: "Builders".to_owned(),
            left: 0,
        },
    );

    let replay = Replay::over(&log.events);
    assert_eq!(replay.tasks.len(), 1);
    assert_eq!(replay.tasks[0].attempts.len(), 1);
    assert_eq!(
        replay.tasks[0].attempts[0].nudges, 1,
        "the stranger's nudge is not this attempt's"
    );
}

/// A question put to a person is filed under the task and stays visible as owed
/// until it is answered — `AwaitingOrders` is not terminal for this reason.
#[test]
fn an_unanswered_question_stays_owed() {
    let mut log = Log::new();
    let (task, attempt) = log.a_task_under_attempt("asked");
    log.after(
        0,
        Event::OperatorPrompted {
            task,
            attempt,
            question: "read it and say what should happen".to_owned(),
        },
    );

    let replay = Replay::over(&log.events);
    let asked = &replay.tasks[0].asked;
    assert_eq!(asked.len(), 1);
    assert!(asked[0].answer.is_none());

    // And answering it fills the same row rather than adding a second.
    let prompt = asked[0].prompt;
    log.after(
        0,
        Event::OperatorAnswered {
            task,
            prompt,
            answer: "the model made no change".to_owned(),
        },
    );
    let replay = Replay::over(&log.events);
    assert_eq!(replay.tasks[0].asked.len(), 1);
    assert_eq!(
        replay.tasks[0].asked[0].answer.as_deref(),
        Some("the model made no change")
    );
}

/// A task the log created and never moved has no state, and that absence is
/// kept rather than assumed to be `Queued`.
#[test]
fn a_task_nothing_moved_has_no_transition_to_report() {
    let mut log = Log::new();
    let m = log.after(
        0,
        Event::MissionCreated {
            title: "m".to_owned(),
        },
    );
    log.after(
        0,
        Event::TaskCreated {
            mission: MissionId::at(m),
            title: "never run".to_owned(),
            prompt: "later".to_owned(),
        },
    );
    // Something that is not about this task at all, so the fold has to ignore it.
    log.after(
        0,
        Event::MissionPhaseEntered {
            mission: MissionId::at(m),
            phase: MissionPhase::Plan,
        },
    );

    let replay = Replay::over(&log.events);
    assert_eq!(replay.tasks.len(), 1);
    assert!(replay.tasks[0].state.is_none());
    assert!(replay.tasks[0].transitions.is_empty());
    assert!(replay.tasks[0].attempts.is_empty());
    assert!(
        replay.endings().is_empty(),
        "a task with no state is in no row"
    );
}

/// 🚨 **F718: the fold must not call eleven thousand unseeded events seeded.**
///
/// `Event::ModelCallStarted::seed` is `#[serde(default)]` over a `u32`, a type
/// with no vacant value — so every one of the **1,910** model calls this project
/// logged before F715 replays as `seed: 0`. A tally that counted rows rather
/// than reading them would report the entire archive as *pinned to one seed*,
/// which is the exact inverse of what F715 found: those calls were flown at the
/// server's own sampling, where five identical requests gave five distinct
/// answers.
///
/// ⚠ And the reading is a convention, not a proof. `seed_for` maps llama.cpp's
/// *choose your own* sentinel to 0, so a derived seed genuinely can be zero —
/// once in 2^32 calls. The fold therefore reports *no seed recorded* and the
/// screen says so; neither says *unseeded*.
#[test]
fn a_pre_seed_attempt_is_not_counted_as_pinned_to_one_seed() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("flown before F715");
    for _ in 0..4 {
        log.after(
            10,
            Event::ModelCallStarted {
                attempt,
                provider: "local".to_owned(),
                model: "m".to_owned(),
                head: "Builders".to_owned(),
                head_digest: "9d491d116cf78300".to_owned(),
                ceiling: "exec".to_owned(),
                budget: 8192,
                // What the archive replays as. Not a seed of zero — no seed.
                seed: 0,
                temperature: String::new(),
            },
        );
    }

    let replay = Replay::over(&log.events);
    let spend = &replay.tasks[0].attempts[0].spend;
    assert_eq!(spend.calls, 4, "the calls themselves are still counted");
    assert_eq!(
        spend.seeded_calls, 0,
        "an archive flown at the server's own sampling reads back as seeded"
    );
    assert!(
        spend.seeds.is_empty(),
        "a default that is not a value became one: {:?}",
        spend.seeds
    );
}

/// The other direction, and it is the one a naive fold also gets wrong.
///
/// F715 derives a seed from `(attempt, head digest, round)` and `round` restarts
/// at zero with each phase, so two phases of one attempt posting the *same* head
/// would repeat the triple and hand two different prompts one sampler draw.
/// Nothing in the derivation forbids it — it holds because the roster gives each
/// phase its own head, which is a fact about the charter. Over 96 attempts and
/// 1,910 model calls on this project's log it has never happened.
///
/// 🚨 So `seeds.len()` is kept *apart* from `seeded_calls` rather than being
/// inferred from it: the gap between the two is the only reading that would say
/// so on the day it does, and a fold that stored a count could not.
#[test]
fn two_calls_that_drew_one_seed_are_visible_as_a_gap() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("seeded");
    for seed in [2_859_510_835_u32, 1_612_451_988, 2_859_510_835] {
        log.after(
            10,
            Event::ModelCallStarted {
                attempt,
                provider: "local".to_owned(),
                model: "m".to_owned(),
                head: "Builders".to_owned(),
                head_digest: "9d491d116cf78300".to_owned(),
                ceiling: "exec".to_owned(),
                budget: 8192,
                seed,
                temperature: String::new(),
            },
        );
    }

    let replay = Replay::over(&log.events);
    let spend = &replay.tasks[0].attempts[0].spend;
    assert_eq!(spend.seeded_calls, 3, "three calls carried a seed");
    assert_eq!(
        spend.seeds.len(),
        2,
        "two calls drew the same seed and the fold cannot say so"
    );
}

/// 🚨 **F718: what a tool handed the model, and the absence that is not a zero.**
///
/// F713 put a tool's output on `ToolCallEnded` because it is the prompt surface
/// nothing could read back, and F717 measured the cost — 244 tokens at the
/// median, 984 at the mean, roughly quadrupling an attempt's record. The tally
/// is where an operator sees which tool is spending it.
///
/// ⚠ `None` is counted apart from `Some("")`. The first is *nobody wrote it
/// down*, and it is what all **2,147** pre-F713 tool calls replay as; the second
/// is *the tool said nothing*, which `bash` does on every successful quiet
/// command. Folding the first in as a zero would report the second about two
/// thousand rows.
#[test]
fn a_tools_output_is_tallied_and_an_unrecorded_one_is_not_a_zero() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("reads");
    // 🚨 The first one carries a `·`, which is two bytes and one character, so
    // the assertion below pins the *unit*. `Scrubbed::len()` is `String::len()`
    // and therefore bytes — the right measure for a field whose whole question
    // is what it costs on disk and in a context window, and the wrong one to
    // call characters. On the sortie that found this, the two readings of one
    // attempt's `read_file` output differed by 291.
    let outputs = [
        Some("crates/abcc-tui/src/line.rs · lines 1-444 of 444"),
        Some(""),
        None,
    ];
    for output in outputs {
        log.after(
            5,
            Event::ToolCallStarted {
                attempt,
                tool: "read_file".to_owned(),
                tier: "read".to_owned(),
            },
        );
        log.after(
            5,
            Event::ToolCallEnded {
                attempt,
                tool: "read_file".to_owned(),
                exit: Some(0),
                elapsed_ms: 5,
                unmeasured: None,
                arguments: None,
                output: output.map(|o| Secrets::default().scrub(o).text),
            },
        );
    }

    let replay = Replay::over(&log.events);
    let tool = &replay.tasks[0].attempts[0].tools[0];
    assert_eq!(tool.calls, 3);
    assert_eq!(
        tool.output_bytes, 49,
        "the tally is not in bytes — 48 means somebody counted characters"
    );
    assert_eq!(
        tool.output_unrecorded, 1,
        "a call nobody recorded was folded in as a tool that returned nothing"
    );
}

// ---------------------------------------------------------------------------
// W13's ladder
// ---------------------------------------------------------------------------

/// 🚨 **The archive could not state the one measurement the milestone is judged
/// on**, and this is the fold that fixes it.
///
/// `abcc review` has written the event since Skeleton and the live screen has
/// rendered it, but `abcc replay` — the instrument this project reaches for when
/// it wants a number about its own history — folded it to nothing. Read the
/// claims below rather than the arithmetic: each one is a decision about what a
/// review *means*, and each would go wrong quietly.
#[test]
fn the_ladder_folds_reviews_onto_the_change_and_not_onto_the_event() {
    let mut log = Log::new();
    log.after(
        0,
        Event::ReviewRecorded {
            change: "223ae3a".to_owned(),
            seconds: 600,
            by: "david".to_owned(),
            crossed_boundary: true,
        },
    );
    // The same change, read again by somebody else, with the boundary flag left
    // off — which is the ordinary shape, because `--by` exists for exactly this.
    log.after(
        60_000,
        Event::ReviewRecorded {
            change: "223ae3a".to_owned(),
            seconds: 300,
            by: "operator".to_owned(),
            crossed_boundary: false,
        },
    );
    log.after(
        60_000,
        Event::ReviewRecorded {
            change: "d6c60e0".to_owned(),
            seconds: 120,
            by: "david".to_owned(),
            crossed_boundary: false,
        },
    );

    let ladder = Replay::over(&log.events).ladder;

    // 🚨 The claim. Two changes, three recordings — and the two numbers are kept
    // apart precisely because they differ here.
    assert_eq!(ladder.changes.len(), 2, "the unit is the change");
    assert_eq!(ladder.recordings, 3, "the passes stay countable");

    let first = &ladder.changes[0];
    assert_eq!(
        first.change, "223ae3a",
        "in the order the log first read them"
    );
    assert_eq!(first.seconds, 900, "the minutes sum onto the change");
    assert_eq!(first.recordings, 2);
    assert_eq!(
        first.by,
        vec!["david".to_owned(), "operator".to_owned()],
        "both readers, distinct, in first-recording order"
    );
    // Sticky, and only in the true direction: whether a change crossed a module
    // boundary is a property of its diff. One reviewer noticing it does not stop
    // being true because the next one left the flag off.
    assert!(first.crossed_boundary);
    assert!(!ladder.changes[1].crossed_boundary);

    assert_eq!(ladder.total_seconds(), 1020);
    assert_eq!(ladder.crossed(), 1, "M3 counts the boundary-crossing ones");
    // The median is per change and never per recording: 900 and 120, so 510.
    assert_eq!(ladder.median_seconds(), Some(510));
}

/// ⚠ An empty ladder is an **absence**, and says so as one.
///
/// F721's trap in its own words: a `0` where a type has no vacant value reads as
/// a measurement. The median of no changes is `None` rather than `0`, because
/// zero is also a legal median — a change somebody looked at for under thirty
/// seconds rounds to it.
#[test]
fn an_empty_ladder_is_an_absence_and_not_a_zero() {
    let mut log = Log::new();
    log.a_task_under_attempt("a task with no review anywhere near it");
    let ladder = Replay::over(&log.events).ladder;

    assert!(ladder.changes.is_empty());
    assert_eq!(ladder.recordings, 0);
    assert_eq!(ladder.total_seconds(), 0);
    assert_eq!(
        ladder.median_seconds(),
        None,
        "no changes is not zero minutes"
    );
    assert_eq!(ladder.crossed(), 0);
}

/// ⚠ **The fold does not resolve a change name**, and this pins it rather than
/// leaving it to be discovered.
///
/// The operator types the string: a short sha, a long sha, a task id, a tag.
/// Nothing here can ask the version control whether two of them are one commit,
/// so two spellings are two rows — and a ladder that silently merged them would
/// be claiming a resolution it never performed.
#[test]
fn two_spellings_of_one_commit_are_two_rows_because_the_log_cannot_tell() {
    let mut log = Log::new();
    for change in ["f3ce25c", "f3ce25c0d9f2"] {
        log.after(
            0,
            Event::ReviewRecorded {
                change: change.to_owned(),
                seconds: 60,
                by: "david".to_owned(),
                crossed_boundary: false,
            },
        );
    }
    let ladder = Replay::over(&log.events).ladder;
    assert_eq!(ladder.changes.len(), 2);
    assert_eq!(ladder.recordings, 2);
}

// ---------------------------------------------------------------------------
// F748 — a cut prompt reaches the after-action view
// ---------------------------------------------------------------------------

/// 🚨 **F744's complaint was that `abcc replay` could not say this happened.**
///
/// Two cuts against one high water, and the projection keeps the count and the
/// deepest of them. ⚠ The worst is `high_water - reported` and is a **floor**:
/// everything appended since the high-water call is missing from that prompt too,
/// and nothing on the log measures that part.
#[test]
fn a_cut_prompt_is_counted_and_its_worst_kept() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("cut");
    for (reported, high_water) in [(19_181_u32, 36_737_u32), (9_965, 36_737)] {
        log.after(
            0,
            Event::PromptCut {
                attempt,
                reported,
                high_water,
            },
        );
    }

    let a = &Replay::over(&log.events).tasks[0].attempts[0];
    assert_eq!(a.spend.prompt_cuts, 2);
    assert_eq!(a.spend.prompt_cut_worst, 26_772);
}

/// The reading the field needs, and the reason it is documented rather than
/// merely present: **zero is *no cut recorded*, not *no cut*.** Every one of this
/// project's 1,977 logged model calls predates the detector, 74 of them were
/// shown a cut prompt (F749), and all 98 attempts replay as zero here.
#[test]
fn an_attempt_from_before_the_detector_counts_no_cuts() {
    let mut log = Log::new();
    let (_, attempt) = log.a_task_under_attempt("older");
    log.after(
        0,
        Event::ModelCallEnded {
            attempt,
            usage: usage(96, None),
            finish: Finish::ToolCalls,
            ttfb_ms: 100,
            elapsed_ms: 200,
            composition: None,
        },
    );

    let a = &Replay::over(&log.events).tasks[0].attempts[0];
    assert_eq!(a.spend.calls, 0, "no call was started, only ended");
    assert_eq!(a.spend.prompt_cuts, 0);
}
