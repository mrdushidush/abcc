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

use abcc_core::event::{Composition, Control, Event, Finish, Usage};
use abcc_core::outcome::Why;
use abcc_core::redact::{MARKER, Secrets};
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::provider::{Delta, Message, ProviderError, Role, TraceSignal};
use abcc_engine::scripted::{Script, Scripted};
use abcc_engine::tools::ToolSpec;
use abcc_engine::workspace::Workspace;
use abcc_engine::{
    Body, ControlPoint, Head, Keep, Limits, NoTools, PhaseEnded, Schema, ToolCall, ToolResult,
    Tools, TurnLoop,
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
        [
            "model_call_started",
            "model_call_ended",
            "claim_recorded",
            // F513: every exit of the phase writes its accounting, including the
            // one that succeeded. A record that counts only failures under-reports.
            "phase_ended",
        ]
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

/// 🚨 **A role defined by having no tools is refused every one of them, and the
/// tool layer is never reached.** The Judge is one model call with no tools
/// (ADR-0002, ADR-0014 §1), which is why it is the phase that may not refuse —
/// and `NoTools` makes the absence a fact about the call rather than a fact
/// about the argument somebody remembered to pass.
///
/// ⚠ The absence is not a failure of the phase: the model asks, is told, and
/// goes on to answer.
#[test]
fn the_review_head_is_refused_every_tool_and_the_tool_layer_is_never_reached() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"src/lib.rs"}"#),
        Script::says(r#"{"assessment":"reviewed from what I was given","findings":[]}"#),
    ]);
    let tools = NoTools::for_head(Head::Commandos);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("review this change");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Commandos,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert!(
        matches!(ended, PhaseEnded::Answered { .. }),
        "a denied tool ended the review: {ended:?}"
    );
    assert_eq!(ended.report().denials, 1);
    assert_eq!(ended.report().tool_calls, 0);

    let Some(Event::ToolCallEnded { unmeasured, .. }) =
        log.iter().find(|e| e.kind() == "tool_call_ended")
    else {
        panic!("the call was not recorded")
    };
    match unmeasured {
        Some(Why::Denied {
            role,
            tool,
            ceiling,
        }) => assert_eq!(
            (role.as_str(), tool.as_str(), ceiling.as_str()),
            ("Commandos", "read_file", "no-tools")
        ),
        other => panic!("expected Denied, got {other:?}"),
    }
}

/// ⚠ The schema is part of the request, and a phase whose artifact has a declared
/// shape that sends `None` produces prose that parses today and does not
/// tomorrow — a failure that would look like the model's. So the seam is
/// asserted rather than assumed.
#[test]
fn a_schema_reaches_the_provider_and_none_is_the_default() {
    const SHAPE: Schema = Schema {
        name: "a_shape",
        json: r#"{"type":"object","additionalProperties":false,"required":[],"properties":{}}"#,
    };
    let provider = Scripted::new(vec![Script::says("{}"), Script::says("plain prose")]);
    let tools = NoTools::for_head(Head::Commandos);
    let (mut control, _handle) = ControlPoint::new();

    let mut constrained = Body::opening("answer in the shape");
    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Commandos,
        ATTEMPT,
        Some(SHAPE),
        &mut constrained,
        &mut control,
        &mut |_: Event| {},
    );
    let mut free = Body::opening("answer however");
    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Commandos,
        ATTEMPT,
        None,
        &mut free,
        &mut control,
        &mut |_: Event| {},
    );

    let seen = provider.seen();
    assert_eq!(seen[0].schema.map(|s| s.name), Some("a_shape"));
    assert_eq!(seen[1].schema, None);
}

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

/// 🚨🚨 **F649, and this is the wire: the refusal has to reach the body the
/// NEXT round is sent, through the real tool layer.**
///
/// Everything between the fold and the model is covered elsewhere one hop at a
/// time — `Patch::parse` returns it, `Workspace::apply_patch` prefixes it,
/// `refused` puts it in `text`. None of those is the claim. The claim is that a
/// model whose `apply_patch` was folded by the server reads, in its own
/// transcript, the name of a tool that works; and the only way to assert that is
/// to run the loop over a real `Workspace` and look in the body.
///
/// ⚠ The `Recorder` used by every other test in this file cannot make this
/// claim: it answers with a fixed string and never reaches `patch.rs` at all.
///
/// ⚠ **Measured rather than assumed, because the first version of this comment
/// was wrong.** Blanking the tool result at `turn.rs` and running the *whole*
/// suite fails two tests: this one, and `http.rs`'s
/// `the_second_request_carries_the_assistant_turn_that_asked_for_the_tool`. So
/// the last hop was not uncovered — what was uncovered is the *join*: that hop
/// carries a stub's fixed string across the HTTP seam and never enters
/// `patch.rs`, and `patch.rs`'s own tests stop at the `ToolResult`. Nothing ran
/// a folded payload through the real tool layer and looked in the body, which is
/// the only place the sentence has to arrive to be worth writing.
#[test]
fn a_folded_apply_patch_puts_write_file_in_the_model_s_own_transcript() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("first.txt"), "alpha\nbeta\n").expect("write");
    let workspace = Workspace::open(dir.path()).expect("open");

    // The payload as the server hands it over (F641): one call's diff, then the
    // opening of a second call, in one `diff` argument.
    let folded = "--- a/first.txt\n+++ b/first.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n\
                  <tool_call>\n<function=apply_patch>\n";
    let provider = Scripted::new(vec![
        Script::calls(
            "c1",
            "apply_patch",
            &serde_json::json!({ "diff": folded }).to_string(),
        ),
        Script::says("understood"),
    ]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("make the test pass");
    let mut log: Vec<Event> = Vec::new();

    // Builders, because that is the head that gets `apply_patch` at all.
    TurnLoop::new(&provider, &workspace, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let told = body
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the refusal reached the transcript");
    assert!(
        told.content.contains("tool-call markup"),
        "{}",
        told.content
    );
    assert!(
        told.content.contains("write_file"),
        "the model reads the refusal and is offered no way out: {}",
        told.content
    );
    // And it really was refused rather than applied: the file is untouched.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("first.txt")).expect("read"),
        "alpha\nbeta\n"
    );
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
    let provider = Scripted::new(vec![Script::truncated_at_cap(Head::Commandos.budget())]);
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
            assert_eq!(
                why,
                Why::TruncatedAtCap {
                    budget: Head::Commandos.budget()
                }
            );
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
            "tool_call_ended",
            // A phase the operator stopped still spent tokens, so it still
            // reports — the same reason `PhaseReport` is returned on a stop.
            "phase_ended",
        ]
    );
}

/// 🚨 **F645: `tool_call_gap` is arithmetic, not taste — so this asserts the
/// arithmetic and not the number.**
///
/// LM Studio buffers a tool call's arguments and delivers them in one delta at
/// the end (F622), so the announced-but-silent window is the whole time the
/// model spends writing the call, and `Head::budget` bounds how many tokens that
/// can be. The gap must cover **the entire budget at the slowest decode this
/// machine has produced** — F622's `bigarg` capture, 9,743 completion tokens
/// across 226.0 s of stream — or the transport goes back to capping how large a
/// patch this system can write, which is the defect F625 names.
///
/// ⚠ **The upper bound is asserted too, and it is the other half of the trade:**
/// every second above what the budget needs is a second a server that died
/// mid-call goes unnoticed. 833 logged calls put the floor at **72.2 tok/s** for
/// calls of this size, so the capture's 43.1 tok/s already carries the margin —
/// piling more on top buys nothing and costs detection.
#[test]
fn the_tool_call_gap_covers_the_whole_budget_at_the_slowest_measured_decode() {
    // F622's `bigarg`: usage.completion_tokens against total_ms - first_byte_ms.
    const SLOWEST_TOKENS: f64 = 9_743.0;
    const SLOWEST_SECONDS: f64 = 226.0;

    let budget = Head::Builders.budget();
    let needed = f64::from(budget) * SLOWEST_SECONDS / SLOWEST_TOKENS;
    let gap = Limits::default().tool_call_gap.as_secs_f64();

    assert!(
        gap >= needed,
        "a {gap:.0} s gap caps the budget: {budget} tokens at {:.1} tok/s needs {needed:.0} s",
        SLOWEST_TOKENS / SLOWEST_SECONDS,
    );
    assert!(
        gap <= needed * 1.25,
        "a {gap:.0} s gap is {:.0}% over the {needed:.0} s the budget needs, and every second \
         of that overshoot is a dead server going unnoticed",
        (gap / needed - 1.0) * 100.0,
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

    // ⚠ `nudges: 0` because this test is about the *signal*. With the default
    // the phase would ask again (F503) and the script would run out, which
    // would be a test of the fixture rather than of ADR-0010 §7.
    let ended = TurnLoop::new(&provider, &tools, MODEL)
        .limits(Limits {
            nudges: 0,
            ..Limits::default()
        })
        .run(
            Head::Commandos,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |_: Event| {},
        );
    assert_eq!(ended.report().trace, TraceSignal::OpenAt200);
    // 🚨 The signal is still a record and not a refusal: the phase does end,
    // but it ends naming **the empty answer** (F497) rather than the trace. No
    // `Why` in this codebase is derived from a `TraceSignal`, which is what
    // ADR-0010 §7 asks for and what this assertion pins.
    let PhaseEnded::Unmeasured { why, .. } = &ended else {
        panic!("a turn that reasoned and said nothing is not an answer: {ended:?}");
    };
    assert!(
        matches!(why, Why::SaidNothing { .. }),
        "the ending must name the absence, not the trace: {why:?}"
    );
}

/// 🚨 **F497.** The model stops cleanly and the payload is empty. That is an
/// absence, and the phase must say so rather than hand the next phase an
/// artifact that is zero characters long.
///
/// ⚠ This shape used to be [`Script::trace_still_open`], and the test above
/// asserted `Answered` on it — which is exactly the defect that shipped: both
/// `ClaimRecorded` events on the first clean run were empty strings, and
/// Localize's empty answer became Change's *"What Recon reported"* input.
#[test]
fn a_phase_whose_model_said_nothing_is_unmeasured_rather_than_answered() {
    // Three, because the phase asks twice more before it gives up (F503). A
    // model that says nothing three times running is the case this ending is
    // for.
    let provider = Scripted::new(vec![
        Script::all_trace_no_answer(47, 42),
        Script::all_trace_no_answer(47, 42),
        Script::all_trace_no_answer(47, 42),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert_eq!(
        kinds(&log).iter().filter(|k| **k == "phase_nudged").count(),
        2,
        "the phase gave up without asking again: {:?}",
        kinds(&log)
    );

    let PhaseEnded::Unmeasured { why, .. } = &ended else {
        panic!("an empty payload is an absence, not an answer: {ended:?}");
    };
    assert_eq!(
        why,
        &Why::SaidNothing {
            by: "Recon".to_owned(),
            completion_tokens: 47,
            reasoning_tokens: Some(42),
        }
    );
    // 🚨 And no claim on the log. An empty `ClaimRecorded` is the absence
    // wearing the shape of an artifact, which is the whole of F497.
    assert!(
        !kinds(&log).contains(&"claim_recorded"),
        "a phase that said nothing wrote a claim anyway: {:?}",
        kinds(&log)
    );
}

/// 🚨 **F496 and F498.** A `length` finish below our own cap was the server's
/// window closing, and the window is `prompt + completion` exactly.
///
/// It reached the log once as `EngineError` → `HardFailure` — *another attempt
/// would repeat this unchanged* — which is the one thing that is not true of a
/// conversation that runs fine against a larger window.
#[test]
fn a_length_finish_below_our_cap_is_the_servers_window_and_names_it() {
    let provider = Scripted::new(vec![Script::filled_the_window(14_261, 2_123)]);
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

    let PhaseEnded::Unmeasured { why, .. } = &ended else {
        panic!("filling the window is not an answer: {ended:?}");
    };
    assert_eq!(
        why,
        &Why::ContextOverflow {
            window: 16_384,
            prompt_tokens: 14_261,
        },
        "the window is measured from the two counts, not guessed"
    );
}

/// The other half of the discriminator, which is what keeps the branch above
/// from swallowing every truncation: a turn that spent **our whole cap** was cut
/// by us, and stays [`Why::TruncatedAtCap`].
///
/// ⚠ The fixture spends **exactly the head's budget**, so `completion < budget`
/// is false and the overflow branch must not fire. It is written as
/// `Head::budget()` rather than as the number: raising the budget from 8,192 to
/// 16,384 turned the literal version of this test into a `ContextOverflow` test
/// without changing a line of it.
#[test]
fn a_length_finish_at_our_own_cap_is_still_the_cap() {
    let provider = Scripted::new(vec![Script::truncated_at_cap(Head::Builders.budget())]);
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

    let PhaseEnded::Unmeasured { why, .. } = &ended else {
        panic!("a turn cut at the cap produced no artifact: {ended:?}");
    };
    assert_eq!(
        why,
        &Why::TruncatedAtCap {
            budget: Head::Builders.budget()
        }
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

/// 🚨 **F503, and the reason the nudge exists at all.** A model that says nothing
/// once is a *sample*, not a verdict: across seven runs of one task the closing
/// answer was missing five times and arrived twice — 2,008 and 3,431 characters
/// — from the same head, brief, server and model.
///
/// So the phase asks again, and the recovered answer is an ordinary `Answered`
/// with an ordinary claim. ⚠ The empty turn stays on the body: hiding it would
/// ask the model to answer a question it cannot see it has already failed.
#[test]
fn a_phase_that_says_nothing_and_then_answers_is_answered() {
    let provider = Scripted::new(vec![
        Script::all_trace_no_answer(61, 52),
        Script::says("crates/abcc/src/cli.rs, at the CliError enum."),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let PhaseEnded::Answered { text, .. } = &ended else {
        panic!("the nudged answer did not become the phase's artifact: {ended:?}");
    };
    assert_eq!(text, "crates/abcc/src/cli.rs, at the CliError enum.");
    assert_eq!(
        kinds(&log).iter().filter(|k| **k == "phase_nudged").count(),
        1,
        "the repair left no trace on the log: {:?}",
        kinds(&log)
    );
    // The absence and the question are both in the transcript the second turn
    // saw — an empty assistant turn, then the user asking for the answer.
    let roles: Vec<Role> = body.messages().iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant, Role::User],
        "the nudged body is not the conversation that happened: {roles:?}"
    );
    assert!(
        body.messages()[1].content.is_empty(),
        "the empty turn was rewritten rather than recorded"
    );
}

/// A tool that refuses, the way `apply_patch` refused five times running.
struct Refuses;

impl Tools for Refuses {
    fn run(&self, spec: &'static ToolSpec, _call: &ToolCall) -> ToolResult {
        ToolResult {
            text: format!("{}: the context is nowhere in the file", spec.name),
            exit: None,
            elapsed_ms: 1,
            unmeasured: Some(Why::FailedBeforeRunning {
                detail: "hunk 1 claims line 123 and its context is nowhere".to_owned(),
            }),
        }
    }
}

/// 🚨 **F505.** A refused write is diagnosable from the log alone.
///
/// Five consecutive `apply_patch` refusals were once readable only as a class:
/// the log carried the tool, the tier and the `Why`, and not the diff that was
/// refused — so the only way to see it was to make the model produce it again.
///
/// ⚠ And the other half, which is what keeps the log from becoming a second copy
/// of the workspace: **a call that succeeded carries no arguments.**
#[test]
fn a_refused_tool_call_keeps_its_arguments_and_a_successful_one_does_not() {
    let patch = "--- a/crates/abcc/src/cli.rs\n+++ b/crates/abcc/src/cli.rs\n@@ -123,3 +123,4 @@\n";
    let arguments = format!(
        "{{\"diff\":{}}}",
        serde_json::to_string(patch).expect("json")
    );

    let refused = Scripted::new(vec![
        Script::calls("c1", "apply_patch", &arguments),
        Script::says("I could not apply it."),
    ]);
    let mut log = Vec::new();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    TurnLoop::new(&refused, &Refuses, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let kept = log
        .iter()
        .find_map(|e| match e {
            Event::ToolCallEnded {
                arguments: Some(a), ..
            } => Some(a.clone()),
            _ => None,
        })
        .expect("the refused call kept nothing to diagnose");
    assert!(
        kept.as_str().contains("@@ -123,3 +123,4 @@"),
        "the refused diff is not on the log: {kept}"
    );

    // The same call, admitted and run: nothing to keep.
    let ok = Scripted::new(vec![
        Script::calls("c1", "apply_patch", &arguments),
        Script::says("done"),
    ]);
    let mut log = Vec::new();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    TurnLoop::new(&ok, &Recorder::default(), MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );
    assert!(
        log.iter().all(|e| !matches!(
            e,
            Event::ToolCallEnded {
                arguments: Some(_),
                ..
            }
        )),
        "a successful call copied its arguments onto the log"
    );
}

/// 🚨 **F506.** A turn cut at our own cap **with content and tool calls** is
/// still an absence, and its tool calls are never run.
///
/// This is the case `a422` fell through: `length`, `content_empty: false`,
/// completion exactly the 8,192 budget — so neither the overflow branch
/// (`completion < budget`) nor the old cap branch (`content_empty`) fired, the
/// fragment was executed, `apply_patch` was handed a **zero-character** argument
/// string and blamed for it, and the malformed turn went onto the body. The next
/// request came back HTTP 500 with an empty page. The window was 32,768 and the
/// conversation was 16,401, so nothing here was an overflow and nothing here was
/// the model's fault.
#[test]
fn a_turn_cut_at_our_cap_mid_tool_call_runs_nothing_and_ends_the_phase() {
    let cut = Script::raw(vec![
        Ok(Delta::Opened { ttfb_ms: 12 }),
        Ok(Delta::Text("I will patch cli.rs".to_owned())),
        // The fragment: the arguments were still streaming when the cap hit.
        Ok(Delta::ToolCall(ToolCall {
            id: "c1".to_owned(),
            tool: "apply_patch".to_owned(),
            arguments: String::new(),
        })),
        Ok(Delta::Closed {
            usage: Usage {
                prompt_tokens: 8_209,
                // Exactly the head's budget: below it this is the server's
                // window (ContextOverflow), at it this is ours.
                completion_tokens: Head::Builders.budget(),
                reasoning_tokens: Some(0),
                cached_tokens: None,
            },
            finish: Finish::Length {
                content_empty: false,
            },
        }),
    ]);
    let provider = Scripted::new(vec![cut]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let PhaseEnded::Unmeasured { why, .. } = &ended else {
        panic!("a turn cut mid-tool-call is not an answer: {ended:?}");
    };
    assert_eq!(
        why,
        &Why::TruncatedAtCap {
            budget: Head::Builders.budget()
        }
    );
    assert!(
        tools.ran.lock().expect("lock").is_empty(),
        "a fragment of a tool call was executed: {:?}",
        tools.ran.lock().expect("lock")
    );
    // And nothing malformed reaches the body, which is what the server chokes on.
    assert!(
        !kinds(&log).contains(&"tool_call_started"),
        "the fragment was admitted: {:?}",
        kinds(&log)
    );
}

// ---------------------------------------------------------------------------
// F511 — where a completion went
// ---------------------------------------------------------------------------

/// Pull the composition off the one `ModelCallEnded` in a log.
///
/// ⚠ The double unwrap is the point: the field is `Option` so that events
/// written before it existed still replay (boot is replay), and every event
/// written *now* must carry one. A `None` here is a regression, not a shrug.
fn composition(log: &[Event]) -> Composition {
    log.iter()
        .find_map(|e| match e {
            Event::ModelCallEnded { composition, .. } => Some(composition.clone()),
            _ => None,
        })
        .expect("no model call was recorded")
        .expect("a model call was recorded without saying what it was made of")
}

/// 🚨 **F511.** The usage block says 8,192 tokens and nothing about where they
/// went. This is the turn that made that gap matter: five live Change phases
/// spent 94–98% of the budget on **one tool call's arguments** and were cut, and
/// from `completion_tokens` alone that is indistinguishable from a model writing
/// prose instead of calling a tool. Reading the totals got it backwards once.
#[test]
fn a_completion_is_split_into_text_reasoning_and_arguments() {
    let huge = "x".repeat(30_000);
    let provider = Scripted::new(vec![Script::cut_assembling_a_call(
        Head::Builders.budget(),
        "write_file",
        &huge,
    )]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let c = composition(&log);
    assert_eq!(c.text_chars, 21, "the answer was tiny and it must say so");
    assert_eq!(c.calls.len(), 1);
    assert_eq!(c.calls[0].tool, "write_file");
    assert_eq!(c.calls[0].argument_chars, 30_000);
    // 🚨 The whole point, as an inequality rather than a pair of numbers: the
    // arguments dwarf everything a reader of the usage block could have seen.
    assert!(
        c.calls[0].argument_chars > (c.text_chars + c.reasoning_chars) * 10,
        "a turn whose budget went into arguments must not read as a turn that talked: {c:?}"
    );
}

/// 🚨 The other half of F505's rule, applied to a new path: keep the text
/// **exactly when the log is the only copy of it**. A cut turn runs none of its
/// calls (F506), so nothing else will ever say what was being written — and the
/// front of a cut argument is where the path is.
#[test]
fn a_discarded_turn_keeps_the_arguments_it_was_cut_writing() {
    let provider = Scripted::new(vec![Script::cut_assembling_a_call(
        Head::Builders.budget(),
        "apply_patch",
        "{\"path\":\"crates/abcc/src/cli.rs\",\"diff\":\"@@ -1,2 +1,3 @@",
    )]);
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

    let PhaseEnded::Unmeasured { why, .. } = &ended else {
        panic!("a turn cut at the cap is not an answer: {ended:?}");
    };
    assert!(matches!(why, Why::TruncatedAtCap { .. }), "{why:?}");

    let c = composition(&log);
    let kept = c.calls[0]
        .arguments
        .as_deref()
        .expect("the fragment was discarded with no copy of what it was writing");
    assert!(
        kept.contains("crates/abcc/src/cli.rs"),
        "the file being written must survive the cut: {kept}"
    );
    // F506 still holds: the fragment is recorded, and it is never run.
    assert!(
        !kinds(&log).contains(&"tool_call_started"),
        "a cut call was admitted: {:?}",
        kinds(&log)
    );
}

/// ⚠ The inverse, and it is what keeps the log from becoming a second copy of
/// the workspace: a call that actually ran needs no copy here. Its arguments are
/// on `ToolCallEnded` if it failed (F505) and in the tree if it did not.
#[test]
fn a_turn_that_is_used_does_not_copy_its_arguments() {
    let args = "{\"path\":\"src/lib.rs\"}";
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", args),
        Script::says("src/lib.rs line 1."),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let c = composition(&log);
    assert_eq!(c.calls.len(), 1);
    assert_eq!(
        c.calls[0].argument_chars,
        u32::try_from(args.chars().count()).expect("fits"),
        "the size is always kept"
    );
    assert!(
        c.calls[0].arguments.is_none(),
        "a call that ran was copied into the log as well as the tree"
    );
}

// ---------------------------------------------------------------------------
// F513 — the phase's accounting reaches the log
// ---------------------------------------------------------------------------

/// 🚨 **F513.** `PhaseReport` was built on every ending, printed, and never
/// written down — and `trace` is computed **in flight**, so it is recoverable
/// from no other event. Two `OpenAt200` observations exist in this project's
/// whole history and both survive only because a person read them off a
/// terminal. This test is the reason there will be a third.
#[test]
fn the_trace_signal_reaches_the_log_and_not_only_the_terminal() {
    let provider = Scripted::new(vec![Script::trace_still_open(640)]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL)
        .limits(Limits {
            nudges: 0,
            ..Limits::default()
        })
        .run(
            Head::Commandos,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );

    let logged = log
        .iter()
        .find_map(|e| match e {
            Event::PhaseEnded { trace, by, .. } => Some((*trace, by.clone())),
            _ => None,
        })
        .expect("the phase ended and wrote no accounting");
    assert_eq!(logged.0, TraceSignal::OpenAt200);
    assert_eq!(logged.1, "Commandos", "the log must name who spent this");
    // The log and the returned report are the same fact, which is the only way
    // the printed summary and the durable record cannot drift apart.
    assert_eq!(logged.0, ended.report().trace);
}

/// Every exit writes exactly one accounting — including the exit that worked.
/// A record that counts only failures under-reports, and two writers for one
/// ending is two numbers that can disagree.
#[test]
fn a_phase_writes_its_accounting_exactly_once_however_it_ends() {
    for (name, scripts) in [
        ("answered", vec![Script::says("done")]),
        (
            "cut",
            vec![Script::truncated_at_cap(Head::Builders.budget())],
        ),
        (
            "used a tool then answered",
            vec![
                Script::calls("c1", "read_file", "{\"path\":\"src/lib.rs\"}"),
                Script::says("done"),
            ],
        ),
    ] {
        let provider = Scripted::new(scripts);
        let tools = Recorder::default();
        let (mut control, _handle) = ControlPoint::new();
        let mut body = Body::opening("go");
        let mut log: Vec<Event> = Vec::new();

        let ended = TurnLoop::new(&provider, &tools, MODEL)
            .limits(Limits {
                nudges: 0,
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

        let written: Vec<&Event> = log
            .iter()
            .filter(|e| matches!(e, Event::PhaseEnded { .. }))
            .collect();
        assert_eq!(written.len(), 1, "{name}: {:?}", kinds(&log));
        let Event::PhaseEnded {
            turns,
            tool_calls,
            elapsed_ms: _,
            ..
        } = written[0]
        else {
            unreachable!()
        };
        let r = ended.report();
        assert_eq!((*turns, *tool_calls), (r.turns, r.tool_calls), "{name}");
    }
}

// ---------------------------------------------------------------------------
// 🚨 The redaction boundary (ADR-0014 §5)
// ---------------------------------------------------------------------------

/// A tool that hands back a credential, the way `bash cat .env` would.
struct Leaks {
    text: String,
    refuse: bool,
}

impl Tools for Leaks {
    fn run(&self, _spec: &'static ToolSpec, _call: &ToolCall) -> ToolResult {
        ToolResult {
            text: self.text.clone(),
            exit: if self.refuse { None } else { Some(0) },
            elapsed_ms: 2,
            unmeasured: self.refuse.then(|| Why::FailedBeforeRunning {
                detail: "refused".to_owned(),
            }),
        }
    }
}

/// 🚨 **All three sinks, in one run.** A secret in a tool result reaches the
/// durable log, the console that projects that log, and the model's own
/// context. The donor redacted the two disk sinks and neither of the others
/// (F418), so this asserts the *context* as hard as it asserts the log.
///
/// ⚠ It is not a claim that the redactor catches everything — see
/// `abcc-core/tests/redact.rs`. It is a claim that the boundary is **on the
/// path**, which is the property the donor's good denylist did not have: it
/// hung off `validate_read_path`, and `bash` never called it.
#[test]
fn a_secret_in_a_tool_result_reaches_neither_the_log_nor_the_model() {
    const KEY: &str = "lm-studio-0123456789abcdef";
    let tools = Leaks {
        // 🚨 Three occurrences, and the **third is the load-bearing one**: the
        // first two also match a shape, so a mutation that dropped the literal
        // half of the denylist would still pass a test that had only those. In
        // prose, only the exact half catches it.
        text: format!(
            "ABCC_MODEL_API_KEY={KEY}\nAuthorization: Bearer {KEY}\nthe key is {KEY} by the way"
        ),
        refuse: false,
    };
    let provider = Scripted::new(vec![
        Script::calls("c1", "bash", r#"{"command":"cat .env"}"#),
        Script::says("read it"),
    ]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find the configuration");
    let mut log: Vec<Event> = Vec::new();

    let _ = TurnLoop::new(&provider, &tools, MODEL)
        .secrets(Secrets::default().with_literal(KEY))
        .run(
            Head::Builders,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );

    // Sink 1 and 2: the log, and the console that reads it.
    let on_the_log = format!("{log:?}");
    assert!(
        !on_the_log.contains(KEY),
        "the key is on the durable log: {on_the_log}"
    );

    // Sink 3: the model's own context, which is the one the donor left open.
    let in_context: String = body.messages().iter().map(|m| m.content.as_str()).collect();
    assert!(
        in_context.contains(MARKER),
        "the tool result never reached the body at all: {in_context}"
    );
    assert!(
        !in_context.contains(KEY),
        "the key is in the model's context: {in_context}"
    );

    // And the operator is told a class was removed, without being told what.
    let note = log
        .iter()
        .find_map(|e| match e {
            Event::Note { text } => Some(text.clone()),
            _ => None,
        })
        .expect("nothing on the log says a redaction happened");
    assert!(note.contains("redacted"), "{note}");
    assert!(!note.contains(KEY), "the note quoted the secret: {note}");
}

/// 🚨 **The refused-arguments field is the one that puts a whole file on
/// disk.** F505 keeps a refused call's arguments so five undiagnosable
/// `apply_patch` refusals cannot happen again; a `write_file` whose content is
/// a credentials file is refused exactly as readily, and that field is then a
/// verbatim copy of it. `Scrubbed` is what makes the two rules compose.
#[test]
fn the_arguments_of_a_refused_call_are_scrubbed_before_they_are_kept() {
    const KEY: &str = "sk-live-0123456789abcdefghij";
    let tools = Leaks {
        text: "refused".to_owned(),
        refuse: true,
    };
    let arguments = format!(r#"{{"path":".env","content":"OPENAI_API_KEY={KEY}"}}"#);
    let provider = Scripted::new(vec![
        Script::calls("c1", "write_file", &arguments),
        Script::says("done"),
    ]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("write the config");
    let mut log: Vec<Event> = Vec::new();

    let _ = TurnLoop::new(&provider, &tools, MODEL)
        .secrets(Secrets::default().with_literal(KEY))
        .run(
            Head::Builders,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );

    let kept = log
        .iter()
        .find_map(|e| match e {
            Event::ToolCallEnded {
                arguments: Some(a), ..
            } => Some(a.clone()),
            _ => None,
        })
        .expect("the refused call kept nothing to diagnose");

    // Still diagnosable — F505's whole point — and no longer a copy of the key.
    assert!(
        kept.as_str().contains(".env"),
        "F505 lost: the refused call is undiagnosable: {kept}"
    );
    assert!(
        !kept.as_str().contains(KEY),
        "the key is on the log: {kept}"
    );
}

/// A run with no secret in it is byte-for-byte what it was before the boundary
/// existed. ⚠ This is the test that fails if a shape is widened carelessly: the
/// cost of a false positive is the model being shown `[redacted]` where its own
/// diff used to be, and it would be discovered in the field.
#[test]
fn an_ordinary_tool_result_passes_through_the_boundary_untouched() {
    let tools = Leaks {
        text: "@@ -123,3 +123,4 @@ fn main() {\n+    println!(\"two\");".to_owned(),
        refuse: false,
    };
    let provider = Scripted::new(vec![
        Script::calls("c1", "apply_patch", r#"{"diff":"@@"}"#),
        Script::says("applied"),
    ]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("make the change");
    let mut log: Vec<Event> = Vec::new();

    let _ = TurnLoop::new(&provider, &tools, MODEL)
        .secrets(Secrets::default().with_literal("lm-studio-0123456789abcdef"))
        .run(
            Head::Builders,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );

    let in_context: String = body.messages().iter().map(|m| m.content.as_str()).collect();
    assert!(
        in_context.contains("+    println!(\"two\");"),
        "the boundary ate an ordinary diff: {in_context}"
    );
    assert!(
        !log.iter().any(|e| matches!(e, Event::Note { .. })),
        "a redaction was reported where nothing was removed: {:?}",
        kinds(&log)
    );
}
