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
use abcc_core::redact::{MARKER, Scrubbed, Secrets};
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::provider::{Delta, Message, ProviderError, Role, TraceSignal};
use abcc_engine::scripted::{Script, Scripted};
use abcc_engine::tools::ToolSpec;
use abcc_engine::workspace::Workspace;
use abcc_engine::{
    Body, ControlPoint, Head, Keep, Limits, NoTools, PhaseEnded, Schema, Temperature, ToolCall,
    ToolResult, Tools, TurnLoop,
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
fn a_folded_apply_patch_puts_edit_file_in_the_model_s_own_transcript() {
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
        told.content.contains("edit_file"),
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

/// F849: a call recovered from the reply text is run, and its markup is kept
/// off the transcript, so the body carries the call once, as a call. The
/// recovery is on the log, and no push follows it.
#[test]
fn a_call_recovered_from_the_reply_runs_and_its_markup_leaves_the_transcript() {
    let markup = "<tool_call><function=read_file>\n<parameter=path>\nsolution.ts\n\
                  </parameter>\n</function>\n</tool_call>";
    let provider = Scripted::new(vec![
        Script::raw(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Text(format!("Reading it first.\n{markup}"))),
            Ok(Delta::ToolCall(ToolCall {
                id: "from_reply_0".to_owned(),
                tool: "read_file".to_owned(),
                arguments: r#"{"path":"solution.ts"}"#.to_owned(),
            })),
            Ok(Delta::Closed {
                usage: Usage {
                    prompt_tokens: 64,
                    completion_tokens: 30,
                    reasoning_tokens: None,
                    cached_tokens: None,
                },
                finish: Finish::Stop,
            }),
        ]),
        Script::says("done"),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("fix makeGrid");
    let mut log = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert!(matches!(ended, PhaseEnded::Answered { .. }), "{ended:?}");
    assert_eq!(tools.ran.lock().unwrap().as_slice(), ["read_file"]);
    let roles: Vec<Role> = body.messages().iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
    let asked = &body.messages()[1];
    assert_eq!(asked.content, "Reading it first.");
    assert_eq!(asked.tool_calls.len(), 1);
    assert!(
        log.iter()
            .any(|e| matches!(e, Event::Note { text } if text.starts_with("F849: 1 tool call"))),
        "the recovery left no trace on the log: {:?}",
        kinds(&log)
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
// F713 — a tool's output reaches the log, and it is the text the model read
// ---------------------------------------------------------------------------

/// The `Role::Tool` message the **provider** was handed on the next call.
///
/// 🚨 **The oracle is the provider and never `secrets.scrub`.** Re-deriving the
/// expectation from the function under test is an oracle comparing a thing with
/// itself: it agrees however wrong both halves are. What makes this field worth
/// having is that it equals what went out on the wire, so that is what it is
/// compared against.
fn tool_message_the_provider_received(provider: &Scripted, call: usize) -> String {
    provider.seen()[call]
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap_or_else(|| panic!("no tool result reached the provider on call {call}"))
        .content
        .clone()
}

fn outputs(log: &[Event]) -> Vec<String> {
    log.iter()
        .filter_map(|e| match e {
            Event::ToolCallEnded {
                output: Some(o), ..
            } => Some(o.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// 🚨 **F713.** A tool's output text is a prompt surface — it enters the model's
/// context and steers the next turn — and for every attempt this project has
/// flown, the log kept the exit code and threw the words away. A successful
/// call's *effect* is in the tree; its *words* were nowhere, so *what did the
/// lint actually print* and *was the model shown the file it asked for* could
/// only be answered by making the model produce it again.
#[test]
fn a_tools_output_is_on_the_log_as_the_model_read_it() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"src/lib.rs"}"#),
        Script::says("read it"),
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

    let logged = outputs(&log);
    assert_eq!(logged.len(), 1, "the call's output is not on the log");
    // Non-trivial, so two empty strings cannot agree their way to a pass.
    assert!(
        logged[0].contains("read_file"),
        "the recorded output is not the tool's: {:?}",
        logged[0]
    );
    assert_eq!(
        logged[0],
        tool_message_the_provider_received(&provider, 1),
        "the log's output is not the text the model was sent"
    );
}

/// ⚠ **A denial's refusal text is a result like any other.** `Why::Denied`
/// already carries the role, the tool and the ceiling — but *the class was
/// denied* and *this is the sentence the model read* are different facts, and
/// the second one is the prompt surface. Deriving the second from the first is
/// exactly what F708 was.
#[test]
fn a_refusal_the_model_was_shown_is_on_the_log_too() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "bash", r#"{"command":"cargo test"}"#),
        Script::says("understood"),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("check whether the tests pass");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let logged = outputs(&log);
    assert_eq!(logged.len(), 1, "the refusal is not on the log");
    assert!(
        logged[0].contains("bash"),
        "the recorded refusal names nothing: {:?}",
        logged[0]
    );
    assert_eq!(
        logged[0],
        tool_message_the_provider_received(&provider, 1),
        "the log's refusal is not the sentence the model read"
    );
}

/// 🚨 **The new field is the third disk sink, and it goes through the same one
/// boundary.** F713 puts a tool's whole output on the log verbatim, which is the
/// arrangement ADR-0014 §5 exists to make safe: the scrub happens once, above
/// the fork, so the log and the context take the *same bytes*. A field scrubbed
/// again on its way to the log would be a second denylist that agrees with the
/// first until the day one of them is edited.
#[test]
fn a_secret_in_a_tools_output_is_scrubbed_once_for_both_sinks() {
    const KEY: &str = "lm-studio-0123456789abcdef";
    let tools = Leaks {
        text: format!("the key is {KEY} by the way"),
        refuse: false,
    };
    let provider = Scripted::new(vec![
        Script::calls("c1", "bash", r#"{"command":"cat .env"}"#),
        Script::says("read it"),
    ]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find the configuration");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &tools, MODEL)
        .secrets(Secrets::default().with_literal(KEY))
        .run(
            // Builders, because `bash` is the exec tier and Commandos is capped
            // at no-tools — the denial is ADR-0014's control and it is not what
            // this test is about.
            Head::Builders,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );

    let logged = outputs(&log);
    assert_eq!(logged.len(), 1);
    assert!(
        !logged[0].contains(KEY),
        "F713 put the key on the log: {:?}",
        logged[0]
    );
    assert!(logged[0].contains(MARKER), "{:?}", logged[0]);
    assert_eq!(
        logged[0],
        tool_message_the_provider_received(&provider, 1),
        "the log and the context were scrubbed separately"
    );
}

// ---------------------------------------------------------------------------
// F715 — the sampler was unseeded, and the log could not say otherwise
// ---------------------------------------------------------------------------

fn seeds_on_the_log(log: &[Event]) -> Vec<u32> {
    log.iter()
        .filter_map(|e| match e {
            Event::ModelCallStarted { seed, .. } => Some(*seed),
            _ => None,
        })
        .collect()
}

/// 🚨 **F715.** Measured against the champion on 2026-09-12: five identical
/// requests carrying what this engine used to send — `max_tokens` and nothing
/// else — produced **five distinct answers of five**; the same five with a seed
/// produced **one**. So every reliability number this project has quoted was
/// taken at the server's own default sampling, unseeded, and a finding like
/// F657's *4 of 5 versus 1 of 5 on a byte-identical prompt* was unfalsifiable by
/// construction rather than merely unreproduced.
///
/// ▶ The seed reaches the provider **and** the log, and this compares the two
/// against each other rather than either against `seed_for`.
#[test]
fn the_seed_reaches_the_provider_and_the_log_says_which_one() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"src/lib.rs"}"#),
        Script::says("read it"),
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

    let sent: Vec<u32> = provider.seen().iter().map(|s| s.seed).collect();
    assert_eq!(sent.len(), 2, "two calls were made");
    assert_eq!(
        seeds_on_the_log(&log),
        sent,
        "the log's seed is not the seed the provider was given"
    );
    assert!(
        sent.iter().all(|&s| s != u32::MAX),
        "a call asked the server to pick its own seed while the log recorded a \
         number: {sent:?}"
    );
}

/// The log says which temperature every call carried: `0` by default since
/// 2026-09-27, `server` when the loop was told to send none.
#[test]
fn the_log_says_which_temperature_each_call_carried() {
    let temperatures = |limits: Limits| {
        let provider = Scripted::new(vec![Script::says("done")]);
        let tools = Recorder::default();
        let (mut control, _handle) = ControlPoint::new();
        let mut body = Body::opening("go");
        let mut log: Vec<Event> = Vec::new();
        TurnLoop::new(&provider, &tools, MODEL).limits(limits).run(
            Head::Recon,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );
        log.into_iter()
            .filter_map(|e| match e {
                Event::ModelCallStarted { temperature, .. } => Some(temperature),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(temperatures(Limits::default()), vec!["0"]);
    let server = Limits {
        temperature: Temperature::Server,
        ..Limits::default()
    };
    assert_eq!(temperatures(server), vec!["server"]);
}

/// 🚨 **Two rounds of one phase are two seeds**, which is the half that makes
/// this a *recorded* seed rather than a *fixed* one. A constant would have made
/// the second round of a phase re-decode the first — and the loop exists to give
/// the model another go at the same body with more in it.
#[test]
fn every_round_of_a_phase_gets_its_own_seed() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"a"}"#),
        Script::calls("c2", "read_file", r#"{"path":"b"}"#),
        Script::says("done"),
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

    let seeds = seeds_on_the_log(&log);
    assert_eq!(seeds.len(), 3);
    let distinct: std::collections::BTreeSet<u32> = seeds.iter().copied().collect();
    assert_eq!(distinct.len(), 3, "rounds shared a seed: {seeds:?}");
}

/// 🚨 **A retry does not re-decode its parent.** F701 made a retry open on its
/// parent's closing checkpoint, so if it also sampled identically it would walk
/// the same path from the same tree and the two together would be a no-op. The
/// `AttemptId` is in the derivation precisely so that it cannot.
///
/// ⚠ And the same attempt run again **does** repeat, which is the other half:
/// that is what *replayable* means, and a test that only asserted difference
/// would pass on a random number.
#[test]
fn a_retry_seeds_differently_and_a_replay_seeds_the_same() {
    fn seeds_for(attempt: AttemptId) -> Vec<u32> {
        let provider = Scripted::new(vec![
            Script::calls("c1", "read_file", r#"{"path":"a"}"#),
            Script::says("done"),
        ]);
        let tools = Recorder::default();
        let (mut control, _handle) = ControlPoint::new();
        let mut body = Body::opening("go");
        let mut log: Vec<Event> = Vec::new();
        TurnLoop::new(&provider, &tools, MODEL).run(
            Head::Recon,
            attempt,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );
        seeds_on_the_log(&log)
    }

    let parent = seeds_for(AttemptId::at(Seq::new(7)));
    let retry = seeds_for(AttemptId::at(Seq::new(8)));
    let replay = seeds_for(AttemptId::at(Seq::new(7)));

    assert_eq!(
        parent, replay,
        "the same attempt did not re-fly the same way"
    );
    assert_ne!(parent, retry, "a retry inherited its parent's sampling");
}

/// ⚠ **The head's digest is in the derivation, not its name.** Two builds that
/// disagree about the prompt must not agree about the sampler and have the pair
/// read as a replication — which is the F711 lesson one layer down: `head` names
/// a constant *within a build*, and editing a charter leaves every field on the
/// event reading as it did before.
#[test]
fn two_heads_do_not_share_a_seed() {
    fn first_seed(head: Head) -> u32 {
        let provider = Scripted::new(vec![Script::says("done")]);
        let (mut control, _handle) = ControlPoint::new();
        let mut body = Body::opening("go");
        let mut log: Vec<Event> = Vec::new();
        TurnLoop::new(&provider, &NoTools::for_head(head), MODEL).run(
            head,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );
        seeds_on_the_log(&log)[0]
    }
    assert_ne!(first_seed(Head::Recon), first_seed(Head::Builders));
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

// ---------------------------------------------------------------------------
// F748 — the prompt the server cut
// ---------------------------------------------------------------------------

/// Every `PromptCut` in a log, as `(reported, high_water)`.
fn cuts(log: &[Event]) -> Vec<(u32, u32)> {
    log.iter()
        .filter_map(|e| match e {
            Event::PromptCut {
                reported,
                high_water,
                ..
            } => Some((*reported, *high_water)),
            _ => None,
        })
        .collect()
}

/// 🚨 **F748: a body that only grows reported a smaller prompt, and until now
/// nothing said so.**
///
/// The loop appends and never removes — `Body` has no `insert` and no `prepend`,
/// and its one indexed write is B3 eviction, which sets the high water to its
/// own estimate (`tests/evict.rs::an_eviction_is_not_a_cut`) — so `prompt_tokens` cannot fall
/// inside one phase. When it does, the server measured its own cut of the
/// prompt, and F745 measured that
/// happening from outside the program: past the window the reported figure stops
/// tracking the input and falls to roughly half of it, at `200 OK`, with no
/// header and no field saying anything was dropped.
///
/// ⚠ Note what the turn itself says: `tool_calls`, no error, nothing uncertain.
/// That is why `Turn::uncertain` cannot find this and the phase's sequence can.
#[test]
fn a_prompt_the_server_cut_is_recorded_against_the_phases_high_water() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"src/cli.rs"}"#).with_prompt_tokens(1_520),
        Script::calls("c2", "read_file", r#"{"path":"src/run.rs"}"#).with_prompt_tokens(36_737),
        // The call a11598 made: the window filled and the server answered over
        // ~19k of a body that had grown past 36k.
        Script::calls("c3", "read_file", r#"{"path":"src/cli.rs"}"#).with_prompt_tokens(19_181),
        Script::says("found it").with_prompt_tokens(19_261),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find where the trailing comma is dropped");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert!(matches!(ended, PhaseEnded::Answered { .. }));
    // 🚨 Both later calls, not only the fall. After the first cut the body keeps
    // growing and the window does not, so the fourth call is *also* working from
    // a prompt the server cut — and its count is **higher** than the third's, so
    // a detector that compared each call with the one before it would report one
    // cut here and the archive's 74 as 23.
    assert_eq!(cuts(&log), [(19_181, 36_737), (19_261, 36_737)]);
    assert!(
        kinds(&log).contains(&"prompt_cut"),
        "the cut is not on the log: {:?}",
        kinds(&log)
    );
}

/// The negative control, and it is the one that makes the detector worth having:
/// a phase that fits reports nothing at all.
#[test]
fn a_prompt_that_only_grows_is_never_reported_as_cut() {
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"src/cli.rs"}"#).with_prompt_tokens(1_520),
        Script::calls("c2", "search", r#"{"pattern":"comma"}"#).with_prompt_tokens(3_230),
        Script::says("src/parser.rs:88").with_prompt_tokens(3_283),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find it");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert_eq!(cuts(&log), [], "a growing prompt was read as a cut one");
}

/// 🚨 **The phase is the unit of monotonicity, and F750 is what happens when it
/// is not.**
///
/// `abcc-drive` builds a fresh `Body::opening` for every phase, so a new phase's
/// first call reports the head and one brief — a fall of tens of thousands of
/// tokens that is the design working. Keyed on the *attempt*, this project's
/// archive reported 117 such falls and **103 of them were phase boundaries**,
/// including the largest, which was quoted as a 38,006-token truncation and is a
/// `change` → `judge` body reset. The loop cannot make that mistake, because the
/// high water lives on the phase's own report and a new phase starts a new one —
/// and this test is what says so.
#[test]
fn a_new_phase_starts_its_own_high_water_and_reports_no_cut() {
    let localize = Scripted::new(vec![
        Script::calls("c1", "read_file", r#"{"path":"src/run.rs"}"#).with_prompt_tokens(18_264),
        Script::says("it is in run.rs").with_prompt_tokens(18_995),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut first = Body::opening("localize the defect");
    let mut log: Vec<Event> = Vec::new();
    TurnLoop::new(&localize, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut first,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    // The next phase, as the driver runs it: its own body, its own loop.
    let change = Scripted::new(vec![Script::says("changed it").with_prompt_tokens(3_056)]);
    let mut second = Body::opening("make the change");
    TurnLoop::new(&change, &tools, MODEL).run(
        Head::Builders,
        ATTEMPT,
        None,
        &mut second,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert_eq!(
        cuts(&log),
        [],
        "a phase boundary was filed as a truncation, which is the reading F750 corrects"
    );
}

// ---------------------------------------------------------------------------
// The re-read guard
// ---------------------------------------------------------------------------

/// The header the guard opens every substituted result with. Spelled out here
/// rather than imported, because a test that reads the constant it is asserting
/// on asserts nothing: the claim is that *this string* reaches the model.
const DUPLICATE_HEADER: &str = "abcc: duplicate tool result";

/// A tool layer with a fixed answer long enough to be worth withholding.
///
/// ⚠ [`Recorder`] cannot make any of these claims: its answer is
/// `<output of read_file>`, 22 bytes, which is smaller than the back-reference
/// that would stand in for it and is therefore served whole by the arithmetic
/// floor. That is itself one of the claims below.
struct Canned {
    text: String,
    /// When set, a counter is appended to every answer, so no two calls return
    /// the same bytes — a file that changed between two reads.
    vary: bool,
    calls: Mutex<usize>,
}

impl Canned {
    fn of(bytes: usize) -> Canned {
        Canned {
            text: "x".repeat(bytes),
            vary: false,
            calls: Mutex::new(0),
        }
    }

    fn varying(bytes: usize) -> Canned {
        Canned {
            vary: true,
            ..Canned::of(bytes)
        }
    }
}

impl Tools for Canned {
    fn run(&self, _spec: &'static ToolSpec, _call: &ToolCall) -> ToolResult {
        let mut calls = self.calls.lock().expect("lock");
        *calls += 1;
        let text = if self.vary {
            format!("{} {}", self.text, *calls)
        } else {
            self.text.clone()
        };
        ToolResult {
            text,
            exit: Some(0),
            elapsed_ms: 3,
            unmeasured: None,
        }
    }
}

/// Every tool result in the body, in order, as the model would read them.
fn served(body: &Body) -> Vec<&str> {
    body.messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.as_str())
        .collect()
}

fn reads(ids: &[&str]) -> Vec<Script> {
    let mut script: Vec<Script> = ids
        .iter()
        .map(|id| Script::calls(id, "read_file", r#"{"path":"src/semantic.rs"}"#))
        .collect();
    script.push(Script::says("done"));
    script
}

fn drive(provider: &Scripted, tools: &dyn Tools, head: Head) -> (Body, Vec<Event>) {
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find where the dotfile skip lets the env file through");
    let mut log: Vec<Event> = Vec::new();
    TurnLoop::new(provider, tools, MODEL).run(
        head,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );
    (body, log)
}

/// 🚨 **The claim F828 is about.** Thirteen byte-identical whole-file reads
/// filled a 40,960 window inside one attempt and it died having changed nothing.
/// The second identical read of an unchanged file is answered with a
/// back-reference, and the bytes are not repeated.
#[test]
fn an_identical_second_read_is_answered_with_a_back_reference() {
    let provider = Scripted::new(reads(&["c1", "c2"]));
    let tools = Canned::of(4_000);
    let (body, log) = drive(&provider, &tools, Head::Recon);

    let served = served(&body);
    assert_eq!(served.len(), 2, "both calls were answered");
    assert_eq!(served[0].len(), 4_000, "the first read is served whole");
    assert!(
        served[1].starts_with(DUPLICATE_HEADER),
        "the second read was repeated verbatim: {}",
        &served[1][..80.min(served[1].len())]
    );
    assert!(
        served[1].len() < 1_000,
        "the back-reference is not cheaper than the bytes it stands in for"
    );

    // 🚨 **F713.** The record says what the model was shown. A guard that
    // substituted *after* `ToolCallEnded` would leave this field claiming the
    // model read 4,000 bytes it never saw.
    let shown: Vec<&str> = log
        .iter()
        .filter_map(|e| match e {
            Event::ToolCallEnded { output, .. } => output.as_ref().map(Scrubbed::as_str),
            _ => None,
        })
        .collect();
    assert_eq!(shown, served, "the log and the context disagree");

    // How much was withheld is a `Note`, because nothing branches on it and the
    // record above can no longer carry it.
    assert!(
        log.iter().any(|e| matches!(
            e,
            Event::Note { text } if text.contains("re-read guard") && text.contains("4000 bytes")
        )),
        "the guard fired without saying so on the log"
    );
}

/// ⚠ **The escape valve, and it is a design guess with no rate behind it.**
/// `Body` is abcc's local record and is not guaranteed to equal what the server
/// retained, so a correctly-detected repeat can still strand a model whose
/// visible copy the server dropped. The second and third repeats are
/// substituted; the fourth is served whole.
#[test]
fn the_fourth_identical_read_is_served_whole() {
    let provider = Scripted::new(reads(&["c1", "c2", "c3", "c4", "c5"]));
    let tools = Canned::of(4_000);
    let (body, _log) = drive(&provider, &tools, Head::Recon);

    let served = served(&body);
    let whole: Vec<bool> = served.iter().map(|s| s.len() == 4_000).collect();
    assert_eq!(
        whole,
        vec![true, false, false, true, false],
        "the escape valve did not open on the fourth occurrence"
    );
}

/// ✅ **A false positive on content is structurally impossible, and this is why.**
/// A file mutated between two reads no longer returns the same bytes, so the
/// equality test *is* the staleness check — there is nothing else to get right.
#[test]
fn a_result_that_changed_between_reads_is_never_a_duplicate() {
    let provider = Scripted::new(reads(&["c1", "c2", "c3"]));
    let tools = Canned::varying(4_000);
    let (body, _log) = drive(&provider, &tools, Head::Recon);

    for (n, text) in served(&body).iter().enumerate() {
        assert!(
            !text.starts_with(DUPLICATE_HEADER),
            "read {n} was withheld although the bytes had changed"
        );
    }
}

/// Reads answer with 4,000 bytes; `edit_file` fails the way a missed `old_text`
/// does, leaving the file — and so the next read's bytes — unchanged.
struct MissedEdit;

impl Tools for MissedEdit {
    fn run(&self, spec: &'static ToolSpec, _call: &ToolCall) -> ToolResult {
        let text = if spec.name == "edit_file" {
            "edit_file: src/semantic.rs: old_text was not found in the file. Closest match: \
             lines 10-43 (19/34 lines match)."
                .to_owned()
        } else {
            "x".repeat(4_000)
        };
        ToolResult {
            text,
            exit: Some(0),
            elapsed_ms: 3,
            unmeasured: None,
        }
    }
}

/// 🚨 **After a failed edit, the re-read is served whole — once.** Found on the
/// first `abcc chat` over K: the model re-read the file to get the bytes its
/// `old_text` missed, the guard said *you already have this* twice, and the
/// attempt ran out of rounds in `cat -A`. The read after the failed edit gets
/// the bytes; a further read with no edit between is a duplicate again.
#[test]
fn a_re_read_after_a_failed_edit_of_that_file_is_served_whole() {
    let read = r#"{"path":"src/semantic.rs"}"#;
    let provider = Scripted::new(vec![
        Script::calls("c1", "read_file", read),
        Script::calls(
            "c2",
            "edit_file",
            r#"{"path":"src/semantic.rs","old_text":"a","new_text":"b"}"#,
        ),
        Script::calls("c3", "read_file", "{\"path\":\"./src/semantic.rs\"}"),
        Script::calls("c4", "read_file", read),
        Script::says("done"),
    ]);
    let (body, _log) = drive(&provider, &MissedEdit, Head::Builders);

    let served = served(&body);
    assert_eq!(served.len(), 4);
    assert_eq!(served[0].len(), 4_000);
    assert_eq!(
        served[2].len(),
        4_000,
        "the read after the failed edit was withheld"
    );
    assert!(
        served[3].starts_with(DUPLICATE_HEADER),
        "a read with no edit since the last copy must still be a duplicate"
    );
}

/// 🚨 **F830 — the class the guard must not touch, and it was measured.**
/// Of the 150 byte-identical repeats in this project's whole log, 12 are
/// `apply_patch`: ten failure messages and **two** *applied 1 hunk to 1 file*
/// lines. An editing tool's result reports **what just happened**, so answering
/// the second one with *you already have this* would be a false statement about
/// an event — and on those two applied-ok lines it would tell a model its
/// second patch had not landed.
#[test]
fn an_editing_tool_is_never_answered_with_a_back_reference() {
    let patch = r#"{"diff":"--- a/src/semantic.rs\n+++ b/src/semantic.rs\n"}"#;
    let provider = Scripted::new(vec![
        Script::calls("c1", "apply_patch", patch),
        Script::calls("c2", "apply_patch", patch),
        Script::calls("c3", "apply_patch", patch),
        Script::says("done"),
    ]);
    let tools = Canned::of(4_000);
    let (body, _log) = drive(&provider, &tools, Head::Builders);

    let served = served(&body);
    assert_eq!(served.len(), 3);
    for (n, text) in served.iter().enumerate() {
        assert_eq!(
            text.len(),
            4_000,
            "apply_patch result {n} was withheld as a duplicate"
        );
    }
}

/// ⚠ **The floor is arithmetic and not a constant.** Substituting only pays when
/// the back-reference is smaller than what it stands in for; there is no measured
/// size threshold, and inventing one would be a number with nothing behind it.
/// In the log this excludes exactly one repeat — a 78-byte `search` result the
/// note would have made bigger.
#[test]
fn a_result_smaller_than_the_back_reference_is_served_whole() {
    let provider = Scripted::new(reads(&["c1", "c2", "c3"]));
    let tools = Canned::of(40);
    let (body, _log) = drive(&provider, &tools, Head::Recon);

    for (n, text) in served(&body).iter().enumerate() {
        assert_eq!(
            text.len(),
            40,
            "read {n} was replaced by something no smaller than itself"
        );
    }
}

// ---------------------------------------------------------------------------
// The reasoning ceiling
// ---------------------------------------------------------------------------

/// A turn that reasons for `chars` characters and then, if it is ever allowed
/// to get there, answers.
///
/// ⚠ The answer and the `Closed` delta are the point: a stop that merely let the
/// stream finish and then reported it would save nothing, and this script cannot
/// tell the difference unless the tail is there to be missed.
fn thinks_then_answers(chars: usize) -> Script {
    let mut deltas = vec![Ok(Delta::Opened { ttfb_ms: 12 })];
    let chunk = "thinking ".repeat(100);
    let mut spent = 0;
    while spent < chars {
        let take = chunk.len().min(chars - spent);
        deltas.push(Ok(Delta::Reasoning(chunk[..take].to_owned())));
        spent += take;
    }
    deltas.push(Ok(Delta::Text("src/parser.rs:88 is the place.".to_owned())));
    Script::raw(deltas).and(Delta::Closed {
        usage: Usage {
            prompt_tokens: 2_000,
            completion_tokens: 16_384,
            reasoning_tokens: Some(16_384),
            cached_tokens: None,
        },
        finish: Finish::Stop,
    })
}

/// 🚨 **F829 — the whole claim.** A turn that reasons past the ceiling is ended
/// by abcc, mid-stream, and the reason says so rather than borrowing a `Why`
/// about something that happened to us.
#[test]
fn a_turn_that_reasons_past_the_ceiling_is_ended_by_abcc() {
    let provider = Scripted::new(vec![thinks_then_answers(5_000)]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find where the parser drops the trailing comma");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &tools, MODEL)
        .limits(Limits {
            reasoning_ceiling: 1_000,
            ..Limits::default()
        })
        .run(
            Head::Recon,
            ATTEMPT,
            None,
            &mut body,
            &mut control,
            &mut |e: Event| log.push(e),
        );

    match ended {
        PhaseEnded::Unmeasured {
            why: Why::ReasoningRunaway { chars, ceiling },
            ..
        } => {
            assert_eq!(ceiling, 1_000);
            assert!(chars >= 1_000, "ended below its own ceiling at {chars}");
        }
        other => panic!("expected ReasoningRunaway, got {other:?}"),
    }

    // 🚨 **Stopping is dropping** — the loop returns and the stream goes with
    // it, which closes the socket (F200). If the drain had run to the end of the
    // script instead, the turn would have assembled and been recorded.
    assert!(
        !kinds(&log).contains(&"model_call_ended"),
        "the stream ran to completion anyway: {:?}",
        kinds(&log)
    );
    assert!(
        !body
            .messages()
            .iter()
            .any(|m| m.content.contains("parser.rs")),
        "the answer the ceiling was supposed to pre-empt still arrived"
    );
}

/// ✅ **The margin, stated as a test rather than as a comment.** The
/// most-reasoning turn in the 3,048 that produced *something* spent **40,628
/// characters** (10,486 reasoning tokens). At the shipped ceiling it answers, and
/// that 9,372-character gap is what *0 false positives* means.
#[test]
fn the_most_reasoning_productive_turn_ever_logged_still_answers() {
    let provider = Scripted::new(vec![thinks_then_answers(40_628)]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find where the parser drops the trailing comma");

    let ended = TurnLoop::new(&provider, &tools, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |_: Event| {},
    );

    match ended {
        PhaseEnded::Answered { text, .. } => {
            assert!(text.contains("parser.rs:88"), "{text}");
        }
        other => panic!("the default ceiling refused a turn the log says is fine: {other:?}"),
    }
}

/// ⚠ **A stop, not a tier** — and a stop nobody set is a stop nobody can trust.
/// The number is the middle of the 41,000–60,000 plateau, and a test that let it
/// drift would let the plateau drift with it.
#[test]
fn the_shipped_reasoning_ceiling_is_the_middle_of_the_measured_plateau() {
    assert_eq!(Limits::default().reasoning_ceiling, 50_000);
}

/// 🚨 **F831 — the ceiling never throws away work already produced.** `a2065`
/// holds a barren turn at 26,275 reasoning chars and a **productive** one at
/// 34,760, in one attempt, so the two populations the plateau was fitted
/// against are not separable by a threshold. This is the half that can be made
/// safe: a turn that has already said something keeps streaming however long it
/// reasons afterwards.
#[test]
fn a_turn_that_already_said_something_is_never_cut_by_the_ceiling() {
    let mut deltas = vec![
        Ok(Delta::Opened { ttfb_ms: 12 }),
        Ok(Delta::Text("src/parser.rs:88 is the place.".to_owned())),
    ];
    let chunk = "thinking ".repeat(100);
    for _ in 0..30 {
        deltas.push(Ok(Delta::Reasoning(chunk.clone())));
    }
    let provider = Scripted::new(vec![Script::raw(deltas).and(Delta::Closed {
        usage: Usage {
            prompt_tokens: 2_000,
            completion_tokens: 16_384,
            reasoning_tokens: Some(16_384),
            cached_tokens: None,
        },
        finish: Finish::Stop,
    })]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find where the parser drops the trailing comma");

    let ended = TurnLoop::new(&provider, &tools, MODEL)
        .limits(Limits {
            reasoning_ceiling: 1_000,
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
        PhaseEnded::Answered { text, .. } => assert!(text.contains("parser.rs:88"), "{text}"),
        other => panic!("the ceiling discarded a turn that had already answered: {other:?}"),
    }
}

/// ⚠ **And the announcement counts, which is the only reason this is more than
/// a text check.** LM Studio buffers a tool call's arguments to the end (F624),
/// so a turn whose only output is a call looks barren for almost all of its
/// trace; `Delta::ToolCallOpened` arrives first and is what the loop has to go
/// on. A turn that has announced a call is not barren.
#[test]
fn a_turn_that_has_announced_a_tool_call_is_never_cut_by_the_ceiling() {
    let mut deltas = vec![
        Ok(Delta::Opened { ttfb_ms: 12 }),
        Ok(Delta::ToolCallOpened {
            tool: "read_file".to_owned(),
        }),
    ];
    let chunk = "thinking ".repeat(100);
    for _ in 0..30 {
        deltas.push(Ok(Delta::Reasoning(chunk.clone())));
    }
    let provider = Scripted::new(vec![
        Script::raw(deltas)
            .and(Delta::ToolCall(ToolCall {
                id: "c1".to_owned(),
                tool: "read_file".to_owned(),
                arguments: r#"{"path":"src/parser.rs"}"#.to_owned(),
            }))
            .and(Delta::Closed {
                usage: Usage {
                    prompt_tokens: 2_000,
                    completion_tokens: 16_384,
                    reasoning_tokens: Some(16_384),
                    cached_tokens: None,
                },
                finish: Finish::ToolCalls,
            }),
        Script::says("src/parser.rs:88 is the place."),
    ]);
    let tools = Recorder::default();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("find where the parser drops the trailing comma");

    let ended = TurnLoop::new(&provider, &tools, MODEL)
        .limits(Limits {
            reasoning_ceiling: 1_000,
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

    assert!(
        matches!(ended, PhaseEnded::Answered { .. }),
        "the ceiling discarded a turn that had announced a tool call: {ended:?}"
    );
    assert_eq!(tools.ran.lock().unwrap().as_slice(), ["read_file"]);
}
