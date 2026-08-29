//! The properties the outcome type holds, each written as the donor defect it
//! exists to make unrepresentable.

use abcc_core::event::Event;
use abcc_core::outcome::{Claim, Counts, Headline, Measurement, Outcome, Report, Why};

const SHA: &str = "0000000000000000000000000000000000000000";

fn measured(rung: &str, exit: i32, detail: &str, counts: Option<Counts>) -> Outcome {
    Outcome::Measured(Measurement {
        rung: rung.to_owned(),
        sha: SHA.to_owned(),
        exit,
        counts,
        detail: detail.to_owned(),
    })
}

/// v1: a task with files written and no test command is `SUCCESS` at confidence
/// 1.0 — the `elif has_meaningful_output:` branch whose `else` arm is commented
/// *"No tests ran or all tests passed = SUCCESS"*.
#[test]
fn a_report_with_nothing_measured_is_not_a_pass() {
    let r = Report::new();
    assert!(!r.headline().is_pass());
    assert_eq!(r.headline(), Headline::Unverified { missing: vec![] });
    assert_eq!(r.measured_fraction(), (0, 0));
}

/// The best donor's `Skipped` carries "the feature is off", "we are offline" and
/// "there is no checker for a `.md` file" in one variant, and the call site folds
/// it into `Passed` (F348). Here they are distinct values and none is a pass.
///
/// The list is every variant of [`Why`]. If a variant is added and this test is
/// not updated it still passes — so the real guard is the `match` in `Display`,
/// which will not compile without an arm. What this test adds is that no two
/// reasons *render* the same, because an operator reads the rendering.
#[test]
fn the_reasons_for_not_measuring_are_distinguishable_and_none_is_green() {
    let reasons = [
        Why::NoCheckerForArtifact {
            artifact: "docs/status_lifecycle.md".into(),
        },
        Why::CheckerNotOnHost {
            binary: "npm".into(),
        },
        Why::SpawnFailed {
            binary: "python3".into(),
            os_error: "exit 9009 (Store alias)".into(),
        },
        Why::NothingToRun {
            detail: "no tests ran in 0.23s".into(),
        },
        Why::FailedBeforeRunning {
            detail: "1 error in 0.39s".into(),
        },
        Why::Timeout { after_ms: 600_000 },
        Why::BudgetExhausted {
            which: "rounds".into(),
        },
        Why::Cancelled {
            by: "operator".into(),
        },
        Why::TruncatedAtCap { budget: 8192 },
        Why::StaleMeasurement {
            taken_at: "aaaa111".into(),
            now: "bbbb222".into(),
        },
        Why::EngineError {
            detail: "provider returned 200 with no content type".into(),
        },
    ];

    let mut rendered: Vec<String> = reasons.iter().map(ToString::to_string).collect();
    rendered.sort();
    rendered.dedup();
    assert_eq!(rendered.len(), reasons.len(), "two reasons render the same");

    for why in reasons {
        let o = Outcome::Unmeasured {
            rung: "tests".into(),
            why: why.clone(),
        };
        assert!(!o.is_green(), "{why} counted as green");
        assert!(!o.is_red(), "{why} counted as red");

        let mut r = Report::new();
        r.record(o);
        let h = r.headline();
        assert!(!h.is_pass(), "{why} produced {h}");
        // The console line names the rung and the reason, which is the whole
        // requirement: "unverified — tests: ran and found nothing to run".
        let line = h.to_string();
        assert!(line.contains("tests"), "{line}");
        assert!(line.contains(&why.to_string()), "{line}");
    }
}

/// v1: `parse_agent_output` returns `AgentOutput(**data)` straight from the JSON
/// the model emitted whenever it carries `files_created` — the model's own
/// `status` and `confidence` become the task record. Here a claim is a different
/// type, and no amount of it moves the headline.
#[test]
fn what_the_model_said_never_becomes_what_the_host_saw() {
    let mut r = Report::new();
    for text in [
        "Done — both existing tests pass.",
        "Test passes clean, no warnings.",
        "All 6 hidden slug tests passed.",
    ] {
        r.note(Claim {
            by: "coder".into(),
            text: text.into(),
        });
    }
    assert_eq!(r.claims().len(), 3);
    assert!(!r.headline().is_pass());
    assert_eq!(r.measured_fraction(), (0, 0));

    // The claims survive for the operator to read; they are simply not evidence.
    assert!(r.claims().iter().any(|c| c.text.contains("tests pass")));
}

/// W11 F296's anti-pattern: a loop that exhausts its budget returning `Ok(())`.
/// The type has no `Ok(())` to return — exhaustion is a reason a rung produced no
/// measurement, and it is neither pass nor fail.
#[test]
fn exhaustion_is_classified_and_is_neither_pass_nor_fail() {
    let mut r = Report::new();
    r.record(measured("structural", 0, "3 source files changed", None));
    r.record(Outcome::Unmeasured {
        rung: "tests".into(),
        why: Why::BudgetExhausted {
            which: "rounds".into(),
        },
    });

    let h = r.headline();
    assert!(!h.is_pass(), "{h}");
    assert!(matches!(h, Headline::Unverified { .. }), "{h}");
    assert_eq!(r.measured_fraction(), (1, 2));
    assert!(h.to_string().contains("rounds budget exhausted"), "{h}");
}

/// The first red wins and it names its rung, so a type error that takes the suite
/// down with it is one failure and not six.
#[test]
fn the_first_red_is_the_headline_and_it_names_itself() {
    let mut r = Report::new();
    r.record(measured(
        "typecheck",
        101,
        "error[E0308]: mismatched types",
        None,
    ));
    r.record(measured(
        "tests",
        101,
        "error: could not compile",
        Some(Counts {
            run: 6,
            passed: 0,
            failed: 6,
        }),
    ));
    let h = r.headline();
    assert!(
        matches!(&h, Headline::Red { rung, .. } if rung == "typecheck"),
        "{h}"
    );
}

/// `PLAN.md` §5: an empty payload at the token cap is `Uncertain` from day one.
/// 17 of 57 judge calls in Phase 1 ended this way, and they are absences rather
/// than failures — a judge that could not answer must not read as a judge that
/// answered "no".
#[test]
fn an_empty_payload_at_the_token_cap_is_an_absence_not_a_verdict() {
    let mut r = Report::new();
    r.record(Outcome::Unmeasured {
        rung: "judge".into(),
        why: Why::TruncatedAtCap { budget: 8192 },
    });
    let h = r.headline();
    assert!(!h.is_pass(), "{h}");
    assert!(matches!(h, Headline::Unverified { .. }), "{h}");
    // And specifically not red: nothing failed, the answer never arrived.
    assert!(!r.outcomes()[0].is_red());
}

/// The whole record round-trips through the log, because ADR-0009 §7 makes every
/// rung — measured or not — an event, and ADR-0005 makes boot a replay of those
/// events. A type that cannot be read back is a type that survives one process.
#[test]
fn every_outcome_round_trips_through_json() {
    let mut r = Report::new();
    r.record(measured(
        "tests",
        0,
        "3 passed in 0.30s",
        Some(Counts {
            run: 3,
            passed: 3,
            failed: 0,
        }),
    ));
    r.record(Outcome::Unmeasured {
        rung: "lint".into(),
        why: Why::CheckerNotOnHost {
            binary: "ruff".into(),
        },
    });
    r.note(Claim {
        by: "coder".into(),
        text: "all green".into(),
    });

    let json = serde_json::to_string(&r).expect("serialize");
    let back: Report = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(r, back);
    assert_eq!(back.headline(), r.headline());
}

// ---------------------------------------------------------------------------
// The log is durable, so this enum is a schema
// ---------------------------------------------------------------------------

/// 🚨 **A required field added to an event makes every event already on disk
/// undeserializable, and boot is replay.**
///
/// This is not hypothetical and it is not a style point: `composition` (F511)
/// was added as a required field, and the first live run afterwards refused to
/// start — *the log: serializing an event: missing field `composition`* — over
/// 1,200 rows written before it existed. `abcc run` could not boot at all.
///
/// ⚠ And the field is `Option` rather than a zeroed default for the same reason
/// `Usage::reasoning_tokens` is: **not recorded is not the same as recorded
/// zero.** A defaulted `Composition` would state that every historical turn
/// produced no text, no trace and no tool calls — a false claim the log would
/// then repeat to every reader forever, which is the shape of every
/// instrument-that-reports-its-own-failure defect in this project.
#[test]
fn an_event_written_before_a_field_existed_still_replays() {
    // A `model_call_ended` exactly as the log held it before F511.
    let old = r#"{
        "kind": "model_call_ended",
        "attempt": 645,
        "usage": {
            "prompt_tokens": 8052,
            "completion_tokens": 959,
            "reasoning_tokens": 952,
            "cached_tokens": null
        },
        "finish": { "finish": "stop" },
        "ttfb_ms": 1034,
        "elapsed_ms": 3200
    }"#;

    let event: Event = serde_json::from_str(old).expect("an old event must still replay");
    let Event::ModelCallEnded {
        composition, usage, ..
    } = &event
    else {
        panic!("wrong variant: {event:?}");
    };
    assert!(
        composition.is_none(),
        "a turn nobody measured must read as unmeasured, never as a turn that produced nothing"
    );
    assert_eq!(usage.completion_tokens, 959);

    // And the round trip does not invent one either: the field is skipped when
    // absent, so replaying a log does not rewrite its history into a claim.
    let back = serde_json::to_string(&event).expect("serialize");
    assert!(
        !back.contains("composition"),
        "an absent measurement was written back as if it had been taken: {back}"
    );
}
