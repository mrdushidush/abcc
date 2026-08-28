//! The turn loop, driven by a written script.
//!
//! Nothing here needs a resident model, which is deliberate: the box has one
//! card, a swap costs 23.77 s and the quality champion runs at 4.9×, so a suite
//! that needs a GPU is a suite nobody runs before committing. What the scripted
//! provider replays is the delta sequence a real stream produces, so these are
//! claims about the loop rather than about a stub.

use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use abcc_core::event::{Control, Event};
use abcc_core::outcome::Why;
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::provider::{Message, ProviderError, Role, TraceSignal};
use abcc_engine::scripted::{Script, Scripted};
use abcc_engine::tools::ToolSpec;
use abcc_engine::{
    Body, ControlPoint, Head, Keep, Limits, PhaseEnded, ToolCall, ToolResult, Tools, TurnLoop,
};

const ATTEMPT: AttemptId = AttemptId::at(Seq::new(7));
const MODEL: &str = "qwen3.6-35b-a3b-mtp@iq3_s";

/// A tool layer that answers with a fixed string and remembers what it was asked
/// for. It never re-checks the policy, which is the contract: two places deciding
/// one thing is how the donor ended up with a path check that is not in the path.
#[derive(Default)]
struct Recorder {
    ran: Mutex<Vec<String>>,
    /// Something to do while the tool "runs" — used to make a control verb
    /// arrive part-way through a round.
    during: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Tools for Recorder {
    fn run(&self, spec: &'static ToolSpec, _call: &ToolCall) -> ToolResult {
        self.ran.lock().expect("lock").push(spec.name.to_owned());
        if let Some(f) = &self.during {
            f();
        }
        ToolResult {
            text: format!("<output of {}>", spec.name),
            exit: Some(0),
            elapsed_ms: 3,
            unmeasured: None,
        }
    }
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(Event::kind).collect()
}

// ---------------------------------------------------------------------------
// Answering
// ---------------------------------------------------------------------------

/// The ordinary path: one call, one answer, and the answer recorded as a claim
/// rather than as a result.
#[test]
fn a_phase_that_answers_records_the_call_and_the_claim() {
    let provider = Scripted::new(vec![Script::says("src/parser.rs:88 is the place.")]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find where the parser drops the trailing comma");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    match &ended {
        PhaseEnded::Answered { text, report } => {
            assert_eq!(text, "src/parser.rs:88 is the place.");
            assert_eq!(report.turns, 1);
            assert_eq!(report.tool_calls, 0);
        }
        other => panic!("expected an answer, got {other:?}"),
    }
    assert_eq!(
        kinds(&log),
        ["model_call_started", "model_call_ended", "claim_recorded"]
    );
    // 🚨 What the model said is a Claim. There is no path from here to an
    // Outcome, and the loop does not build one.
    let Event::ClaimRecorded { claim, .. } = &log[2] else {
        panic!("not a claim")
    };
    assert_eq!(claim.by, "Recon");
}

/// `PLAN.md` §5: `reasoning_tokens` logged on every call from day one. It is
/// already on the wire, it is the quantity that overruns, and without it the
/// failure is invisible in the record.
#[test]
fn reasoning_tokens_are_carried_on_every_call() {
    let provider = Scripted::new(vec![Script::says("done").with_reasoning_tokens(1_432)]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert_eq!(ended.report().reasoning_tokens, Some(1_432));
    let Event::ModelCallEnded { usage, .. } = &log[1] else {
        panic!("not a model call ending")
    };
    assert_eq!(usage.reasoning_tokens, Some(1_432));
}

// ---------------------------------------------------------------------------
// The freeze
// ---------------------------------------------------------------------------

/// 🚨 **The freeze, end to end.** Across every round of a phase the head is the
/// same bytes at the same address, and each request's messages are a *prefix* of
/// the next — which is ADR-0010's *append the failure context, never prepend it*
/// stated as a property rather than as a rule.
#[test]
fn the_head_never_moves_and_the_body_only_grows() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"src/parser.rs"}"#),
        Script::calls("c2", "search", r#"{"pattern":"trailing"}"#),
        Script::says("found it"),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find the bug");

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    assert!(matches!(ended, PhaseEnded::Answered { .. }));

    let seen = provider.seen();
    assert_eq!(seen.len(), 3);
    for window in seen.windows(2) {
        let (before, after) = (&window[0], &window[1]);
        assert!(
            std::ptr::eq(before.head_prefix, after.head_prefix),
            "the head was rebuilt between rounds"
        );
        assert_eq!(before.head_key, after.head_key);
        assert_eq!(before.budget, after.budget);
        assert_eq!(
            &after.messages[..before.messages.len()],
            &before.messages[..],
            "round {} is not an extension of the one before it",
            after.messages.len()
        );
    }
    assert_eq!(seen[0].messages.len(), 1, "the opening is one message");
    assert_eq!(
        seen[0].messages[0].role,
        Role::User,
        "the head is the system half and never a message"
    );
    assert_eq!(
        tools.ran.lock().unwrap().as_slice(),
        ["read_file", "search"]
    );
}

// ---------------------------------------------------------------------------
// Denial
// ---------------------------------------------------------------------------

/// 🚨 ADR-0014's control, exercised through the loop rather than through the
/// policy alone: Recon asks for a shell, is refused, and **is told** — on the log
/// as `Why::Denied` and in its own transcript as that call's result.
#[test]
fn a_denied_tool_is_refused_on_the_log_and_in_the_transcript() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "bash", r#"{"command":"cargo test"}"#),
        Script::says("understood, I cannot run that"),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("check whether the tests pass");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert_eq!(ended.report().denials, 1);
    assert_eq!(ended.report().tool_calls, 0, "a refused call did not run");
    assert!(
        tools.ran.lock().unwrap().is_empty(),
        "the tool layer was reached for a call the policy refused"
    );

    let ended_events: Vec<&Event> = log
        .iter()
        .filter(|e| e.kind() == "tool_call_ended")
        .collect();
    assert_eq!(ended_events.len(), 1);
    let Event::ToolCallEnded {
        unmeasured, exit, ..
    } = ended_events[0]
    else {
        panic!("not a tool ending")
    };
    assert_eq!(*exit, None, "a refused call has no exit status");
    match unmeasured {
        Some(Why::Denied {
            role,
            tool,
            ceiling,
        }) => {
            assert_eq!(
                (role.as_str(), tool.as_str(), ceiling.as_str()),
                ("Recon", "bash", "read")
            );
        }
        other => panic!("expected Denied, got {other:?}"),
    }

    // The model sees the refusal in its own transcript, so it does not spend the
    // round budget asking again.
    let told = body
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the refusal reached the transcript");
    assert!(told.content.contains("Recon"), "{}", told.content);
    assert!(told.content.contains("bash"), "{}", told.content);
}

/// ⚠ And a `ToolCallStarted` is not written for a call that never started. The
/// log says what happened, not what was asked for.
#[test]
fn a_refused_call_never_reads_as_started() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "write_file", r#"{"path":"x","content":"y"}"#),
        Script::says("noted"),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("edit it");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );
    assert!(!kinds(&log).contains(&"tool_call_started"));
}

// ---------------------------------------------------------------------------
// Endings that are not answers
// ---------------------------------------------------------------------------

/// 🚨 The cap with an empty payload. 17 of 57 judge calls were lost this way, and
/// none of them is a zero — so the phase ends `Unmeasured` and the empty string
/// never reaches a caller as an artifact.
#[test]
fn an_empty_payload_at_the_cap_is_an_absence() {
    let provider = Scripted::new(vec![Script::truncated_at_cap(8_192)]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("review this diff");

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Commandos,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    match ended {
        PhaseEnded::Unmeasured { why, report } => {
            assert_eq!(why, Why::TruncatedAtCap { budget: 8_192 });
            assert_eq!(report.turns, 1, "the turn still happened and still cost");
        }
        other => panic!("expected Unmeasured, got {other:?}"),
    }
}

/// A proxy that answers 200 and then nothing looks exactly like a finished
/// stream. It is named rather than guessed at.
#[test]
fn a_stream_that_ends_without_an_ending_is_malformed_and_not_empty() {
    let provider = Scripted::new(vec![Script::ends_without_saying_so()]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    match ended {
        PhaseEnded::Unmeasured {
            why: Why::EngineError { detail },
            ..
        } => assert!(detail.contains("without a finish reason"), "{detail}"),
        other => panic!("expected an engine error, got {other:?}"),
    }
}

/// A provider fault becomes the class it is, not a generic failure.
#[test]
fn an_idle_gap_reaches_the_report_as_a_timeout() {
    let provider = Scripted::new(vec![Script::fails(ProviderError::IdleGap {
        after_ms: 90_000,
    })]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    assert!(matches!(
        ended,
        PhaseEnded::Unmeasured {
            why: Why::Timeout { after_ms: 90_000 },
            ..
        }
    ));
}

/// ⚠ The round budget is a stop and not a tier: exhausting it escalates nothing,
/// because there is nothing to escalate to.
#[test]
fn the_round_budget_is_a_stop_and_not_a_tier() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", "{}"),
        Script::calls("c2", "read_file", "{}"),
        Script::calls("c3", "read_file", "{}"),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("look forever");

    let ended = TurnLoop::new(&provider, &tools, MODEL)
        .limits(Limits {
            rounds: 2,
            ..Limits::default()
        })
        .run(
            Head::Recon,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |_: Event| {},
        );
    match ended {
        PhaseEnded::Unmeasured {
            why: Why::BudgetExhausted { which },
            report,
        } => {
            assert_eq!(which, "2 rounds");
            assert_eq!(report.turns, 2);
        }
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
    assert_eq!(provider.remaining(), 1, "the third round did not happen");
}

// ---------------------------------------------------------------------------
// The operator
// ---------------------------------------------------------------------------

/// 🚨 An urgent verb stops a stream that is genuinely in flight. The loop samples
/// between deltas and drops the stream; nothing reaches into the provider.
#[test]
fn an_urgent_verb_stops_a_stream_in_flight() {
    let provider = Scripted::new(vec![
        Script::says("a").and(abcc_engine::Delta::Text("b".repeat(64))),
        Script::says("never reached"),
    ])
    .paced(Duration::from_millis(20));
    let tools = Recorder::default();
    let (mut control, handle) = ControlPoint::new();
    let mut body = Body::opening("go");

    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        let _ = handle.request(Control::Kill);
    });

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    stopper.join().expect("the stopper thread did not panic");

    match ended {
        PhaseEnded::Stopped { stop, .. } => {
            assert_eq!(stop.control, Control::Kill);
            assert_eq!(stop.keep, Keep::Nothing);
        }
        other => panic!("expected Stopped, got {other:?}"),
    }
    assert_eq!(
        provider.remaining(),
        1,
        "the loop started another turn after being killed"
    );
}

/// 🚨 A pause lets the work in flight finish. The verb arrives while a tool is
/// running; the round completes, and the *next* boundary is where it stops.
#[test]
fn a_pause_lets_the_round_in_flight_finish() {
    let (mut control, handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", "{}"),
        Script::says("never reached"),
    ]);
    let tools = Recorder {
        ran: Mutex::new(Vec::new()),
        during: Some(Box::new(move || {
            let _ = handle.request(Control::Pause);
        })),
    };
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    match ended {
        PhaseEnded::Stopped { stop, report } => {
            assert_eq!(stop.control, Control::Pause);
            assert_eq!(stop.keep, Keep::AtCheckpoint);
            assert_eq!(report.turns, 1, "the turn in flight did not complete");
            assert_eq!(report.tool_calls, 1, "the tool in flight did not complete");
        }
        other => panic!("expected Stopped, got {other:?}"),
    }
    assert_eq!(
        provider.remaining(),
        1,
        "a pause must not start another turn"
    );
    assert_eq!(
        kinds(&log),
        [
            "model_call_started",
            "model_call_ended",
            "tool_call_started",
            "tool_call_ended"
        ]
    );
}

// ---------------------------------------------------------------------------
// Instrumentation
// ---------------------------------------------------------------------------

/// ADR-0012 §5's bar: no gap over ten seconds goes unmarked. The gap is a limit
/// here so the test can be fast; the shipped default is ten seconds.
#[test]
fn a_quiet_stream_is_marked_alive() {
    assert_eq!(Limits::default().liveness_gap, Duration::from_secs(10));

    let provider = Scripted::new(vec![Script::says("slowly")]).paced(Duration::from_millis(30));
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &tools, MODEL)
        .limits(Limits {
            liveness_gap: Duration::from_millis(20),
            ..Limits::default()
        })
        .run(
            Head::Builders,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );

    assert!(
        kinds(&log).contains(&"liveness_mark"),
        "a stream quiet past its gap went unmarked: {:?}",
        kinds(&log)
    );
}

/// ADR-0010 §7's signal, recorded rather than acted on — and the doc comment on
/// the field says exactly that, so nobody reads the value as a stop that fired.
#[test]
fn an_open_trace_at_token_200_is_recorded() {
    let provider = Scripted::new(vec![Script::trace_still_open(640)]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Commandos,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    assert_eq!(ended.report().trace, TraceSignal::OpenAt200);
    assert!(
        matches!(ended, PhaseEnded::Answered { .. }),
        "the signal is a record at Skeleton, not a refusal"
    );
}

/// A short turn with a trace that closed is not the same thing, and neither is a
/// provider that reports no trace at all.
#[test]
fn a_trace_that_closed_and_a_trace_that_never_existed_are_different_values() {
    let short = Scripted::new(vec![Script::trace_still_open(40)]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let ended = TurnLoop::new(&short, &tools, MODEL).run(
        Head::Commandos,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    assert_eq!(ended.report().trace, TraceSignal::Closed);

    let none = Scripted::new(vec![Script::says("plain")]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let ended = TurnLoop::new(&none, &tools, MODEL).run(
        Head::Commandos,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    assert_eq!(ended.report().trace, TraceSignal::Absent);
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

/// ADR-0013's `&self`, exercised rather than asserted: two loops share one
/// provider by reference, which the inherited `&mut self` signature made
/// impossible.
#[test]
fn one_provider_serves_two_loops_at_once() {
    let provider = Scripted::new(vec![Script::says("first"), Script::says("second")]);
    let tools = Recorder::default();
    let loop_a = TurnLoop::new(&provider, &tools, MODEL);
    let loop_b = TurnLoop::new(&provider, &tools, MODEL);

    let run = |l: &TurnLoop<'_>, head: Head| {
        let (mut control, _handle) = ControlPoint::new();
        let mut body = Body::opening("go");
        l.run(
            head,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |_: Event| {},
        )
    };
    assert!(matches!(
        run(&loop_a, Head::Recon),
        PhaseEnded::Answered { .. }
    ));
    assert!(matches!(
        run(&loop_b, Head::Builders),
        PhaseEnded::Answered { .. }
    ));
    assert_eq!(provider.seen().len(), 2);
    assert_ne!(
        provider.seen()[0].head_key,
        provider.seen()[1].head_key,
        "two heads, one provider"
    );
}

/// The `Message` vocabulary has no `System` variant, so a caller cannot prepend
/// one. This asserts the consequence: every message the provider is handed is a
/// user, assistant or tool turn, and the system half arrives only as the head.
#[test]
fn nothing_in_a_body_can_be_a_system_message() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", "{}"),
        Script::says("ok"),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");

    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );
    for seen in provider.seen() {
        for Message { role, .. } in seen.messages {
            assert!(matches!(role, Role::User | Role::Assistant | Role::Tool));
        }
        assert!(!seen.head_prefix.is_empty(), "the system half is the head");
    }
}
