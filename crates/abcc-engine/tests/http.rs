//! The OpenAI-compatible provider, against a socket that behaves the way F198
//! and F199 measured a server behaving.
//!
//! 🚨 **The first three tests are the ones ADR-0006 calls not optional, and the
//! first of them failed on its first run.** It was written to pin F198's per-read
//! timeout semantic, because `RequestBuilder::timeout`'s doc comment describes
//! the opposite behaviour and *"a future release could align the code to the doc
//! and silently turn 2.0's hang detector into a wall clock"*. It had already
//! happened. Re-measured against this stub with a control that completes:
//! six lines 200 ms apart under a 500 ms budget fail at **0.502 s** on reqwest
//! 0.12.28 and **0.509 s** on 0.13.4 — a total duration, on both (F493).
//!
//! So these tests now pin **our** idle gap rather than the library's, and the one
//! that matters is unchanged in what it asserts: a turn whose body outlasts the
//! budget, with every gap inside it, must complete. If it ever fails again, do
//! not raise the budget — the hang detector has become a wall clock and the
//! instrument needs replacing, which is exactly what it did the first time.
//!
//! The stub is a raw `TcpListener` rather than a server crate: the thing under
//! test is *when bytes arrive*, and a framework that buffers on our behalf would
//! test its own buffering. Delays are scaled down from F198's seconds to
//! milliseconds — the property is a ratio, and the suite is run before every
//! commit.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use abcc_core::event::{Event, Finish};
use abcc_core::outcome::Why;
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::openai::OpenAiCompat;
use abcc_engine::provider::{ApiRequest, Delta, ProviderError, Role, Schema};
use abcc_engine::tools::{TOOLS, ToolSpec};
use abcc_engine::{
    Body, ControlPoint, Head, PhaseEnded, Provider, ToolCall, ToolResult, Tools, TurnLoop,
};
use serde_json::Value;

const ATTEMPT: AttemptId = AttemptId::at(Seq::new(7));
const MODEL: &str = "qwen3.6-35b-a3b-mtp@iq3_s";

/// The budget every timing test is measured against. Small enough that the suite
/// stays cheap, and every delay around it is at least 1.8x on the side that
/// decides the assertion.
const BUDGET: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// A server that writes when it is told to
// ---------------------------------------------------------------------------

/// One reply, as bytes and the pauses between them.
struct Reply {
    status: u16,
    content_type: &'static str,
    /// How long the request waits before any headers come back — F199's test E.
    headers_after: Duration,
    /// `(pause before writing, bytes)`.
    pieces: Vec<(Duration, String)>,
}

impl Reply {
    fn sse(pieces: Vec<(Duration, String)>) -> Reply {
        Reply {
            status: 200,
            content_type: "text/event-stream",
            headers_after: Duration::ZERO,
            pieces,
        }
    }

    fn json(status: u16, body: &str) -> Reply {
        Reply {
            status,
            content_type: "application/json",
            headers_after: Duration::ZERO,
            pieces: vec![(Duration::ZERO, body.to_owned())],
        }
    }

    fn after(mut self, delay: Duration) -> Reply {
        self.headers_after = delay;
        self
    }
}

/// A socket that answers each request with the next scripted reply, and keeps
/// every request body it was sent.
struct Stub {
    base_url: String,
    sent: Arc<Mutex<Vec<String>>>,
    _serving: JoinHandle<()>,
}

impl Stub {
    fn new(replies: Vec<Reply>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&sent);
        let serving = thread::spawn(move || {
            for reply in replies {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                seen.lock().expect("lock").push(read_request(&mut socket));
                thread::sleep(reply.headers_after);
                let head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    if reply.status == 200 { "OK" } else { "Error" },
                    reply.content_type,
                );
                if socket.write_all(head.as_bytes()).is_err() {
                    continue;
                }
                let _ = socket.flush();
                for (pause, piece) in reply.pieces {
                    thread::sleep(pause);
                    if socket.write_all(piece.as_bytes()).is_err() {
                        break;
                    }
                    let _ = socket.flush();
                }
                let _ = socket.shutdown(Shutdown::Write);
            }
        });
        Stub {
            base_url,
            sent,
            _serving: serving,
        }
    }

    fn provider(&self) -> OpenAiCompat {
        OpenAiCompat::new(&self.base_url).expect("build a client")
    }

    /// The request bodies this stub was sent, parsed.
    fn requests(&self) -> Vec<Value> {
        self.sent
            .lock()
            .expect("lock")
            .iter()
            .map(|body| serde_json::from_str(body).expect("the request body is JSON"))
            .collect()
    }
}

/// Read one HTTP request off the socket and return its body.
fn read_request(socket: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    // Headers first, one byte at a time — the request is tiny and this needs no
    // buffering that could swallow the body.
    while !raw.ends_with(b"\r\n\r\n") {
        match socket.read(&mut byte) {
            Ok(0) | Err(_) => return String::new(),
            Ok(_) => raw.push(byte[0]),
        }
    }
    let headers = String::from_utf8_lossy(&raw).to_ascii_lowercase();
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if socket.read_exact(&mut body).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&body).into_owned()
}

// ---------------------------------------------------------------------------
// Convenience
// ---------------------------------------------------------------------------

fn chunk(body: &str) -> String {
    format!("data: {body}\n\n")
}

fn text_chunk(text: &str) -> String {
    chunk(&format!(
        r#"{{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#
    ))
}

const STOP: &str = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
const USAGE: &str = r#"{"choices":[],"usage":{"prompt_tokens":64,"completion_tokens":12}}"#;

fn request(head: Head, body: &Body, idle_gap: Duration) -> ApiRequest<'_> {
    ApiRequest {
        model: MODEL,
        head,
        body,
        schema: None,
        idle_gap,
    }
}

/// Read a whole turn out of a provider, the way the loop does.
fn drain(provider: &OpenAiCompat, req: &ApiRequest<'_>) -> Vec<Result<Delta, ProviderError>> {
    match provider.start(req) {
        Ok(mut stream) => {
            let mut deltas = Vec::new();
            while let Some(delta) = stream.next_delta() {
                deltas.push(delta);
            }
            deltas
        }
        Err(e) => vec![Err(e)],
    }
}

fn only_error(deltas: &[Result<Delta, ProviderError>]) -> &ProviderError {
    deltas
        .iter()
        .find_map(|d| d.as_ref().err())
        .expect("an error in the stream")
}

// ---------------------------------------------------------------------------
// 🚨 The two not-optional tests (ADR-0006)
// ---------------------------------------------------------------------------

/// 🚨 **The budget is a gap, not a wall clock.**
///
/// Six pieces 120 ms apart is 720 ms of body under a 400 ms budget. A total
/// duration fails at 400 ms; a gap completes, because no single silence reaches
/// it. This is the test that caught F493 — it is written against the provider's
/// behaviour rather than against reqwest's, so it holds whatever the library
/// does next, and it fails the moment somebody puts a `timeout()` back on the
/// request.
///
/// If it fails, **do not raise the budget.** A budget that has to be raised to
/// let ordinary work finish is not measuring what it claims to.
#[test]
fn a_turn_whose_body_outlasts_the_budget_completes_while_every_gap_stays_inside_it() {
    let gap = Duration::from_millis(120);
    let mut pieces: Vec<(Duration, String)> = (0..4)
        .map(|i| (gap, text_chunk(&format!("part{i} "))))
        .collect();
    pieces.push((gap, chunk(STOP)));
    pieces.push((gap, chunk(USAGE)));
    pieces.push((gap, "data: [DONE]\n\n".to_owned()));
    let stub = Stub::new(vec![Reply::sse(pieces)]);
    let provider = stub.provider();
    let body = Body::opening("go");

    let started = Instant::now();
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));
    let elapsed = started.elapsed();

    assert!(
        elapsed > BUDGET,
        "the body took {elapsed:?}, which is inside the budget — the test proves nothing"
    );
    let text: String = deltas
        .iter()
        .filter_map(|d| match d {
            Ok(Delta::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "part0 part1 part2 part3 ");
    assert!(
        matches!(deltas.last(), Some(Ok(Delta::Closed { finish, .. })) if *finish == Finish::Stop),
        "the stream did not close cleanly: {deltas:?}"
    );
}

/// The other half of the same claim: a gap **over** the budget is a hang, and it
/// arrives as its own class rather than as a generic transport failure. F198's
/// test A2 shape — a gap past the budget stops at the budget.
#[test]
fn a_gap_longer_than_the_budget_is_an_idle_gap_and_never_a_generic_failure() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::ZERO, text_chunk("half an ans")),
        (Duration::from_millis(900), chunk(STOP)),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");

    let started = Instant::now();
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));
    let elapsed = started.elapsed();

    match only_error(&deltas) {
        ProviderError::IdleGap { after_ms } => assert_eq!(*after_ms, 400),
        other => panic!("expected IdleGap, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_millis(900),
        "it waited {elapsed:?} — the budget did not stop the read"
    );
    // 🚨 And the class survives the trip to the report: the operator reads "the
    // stream was silent", not "something went wrong".
    assert!(matches!(
        only_error(&deltas).why(),
        Why::Timeout { after_ms: 400 }
    ));
}

/// F199's test E: **the same number bounds the wait for the first byte**, and
/// separately from every later read rather than cumulatively. There is no second
/// knob on the blocking client to set it with — `read_timeout` is on the async
/// builder and absent from this one — so the bound is the same `recv_timeout`
/// that bounds every later delta, and silence before the headers is silence.
#[test]
fn the_same_budget_bounds_the_wait_for_the_first_byte() {
    let stub = Stub::new(vec![
        Reply::sse(vec![(Duration::ZERO, chunk(STOP))]).after(Duration::from_millis(900)),
    ]);
    let provider = stub.provider();
    let body = Body::opening("go");

    let started = Instant::now();
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));
    let elapsed = started.elapsed();

    assert!(
        matches!(only_error(&deltas), ProviderError::IdleGap { .. }),
        "expected IdleGap, got {deltas:?}"
    );
    assert!(
        elapsed < Duration::from_millis(900),
        "it waited {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// The delta sequence
// ---------------------------------------------------------------------------

/// The ordinary turn, in the order `scripted.rs` says a stream produces it.
#[test]
fn a_streamed_turn_produces_the_sequence_the_scripted_contract_describes() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (
            Duration::ZERO,
            chunk(r#"{"choices":[{"delta":{"reasoning_content":"thinking"}}]}"#),
        ),
        (Duration::ZERO, text_chunk("done")),
        (Duration::ZERO, chunk(STOP)),
        (
            Duration::ZERO,
            chunk(
                r#"{"choices":[],"usage":{"prompt_tokens":64,"completion_tokens":12,"completion_tokens_details":{"reasoning_tokens":9},"prompt_tokens_details":{"cached_tokens":48}}}"#,
            ),
        ),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    assert!(
        matches!(deltas.first(), Some(Ok(Delta::Opened { .. }))),
        "the first delta names the time to first byte: {deltas:?}"
    );
    assert!(matches!(&deltas[1], Ok(Delta::Reasoning(t)) if t == "thinking"));
    assert!(matches!(&deltas[2], Ok(Delta::Text(t)) if t == "done"));
    match deltas.last() {
        Some(Ok(Delta::Closed { usage, finish })) => {
            assert_eq!(*finish, Finish::Stop);
            assert_eq!(usage.prompt_tokens, 64);
            assert_eq!(usage.completion_tokens, 12);
            // ⚠ Reported, so `Some`. The difference from zero is the whole point
            // of the field.
            assert_eq!(usage.reasoning_tokens, Some(9));
            // The number that says whether the frozen head is doing its job.
            assert_eq!(usage.cached_tokens, Some(48));
        }
        other => panic!("expected a Closed, got {other:?}"),
    }
}

/// Tool calls arrive as fragments — a name in one chunk and the arguments spread
/// over several — and they reach the loop as whole calls, before the ending.
#[test]
fn tool_calls_are_assembled_from_their_fragments() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (
            Duration::ZERO,
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
            ),
        ),
        (
            Duration::ZERO,
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"src/lib.rs\"}"}}]}}]}"#,
            ),
        ),
        (
            Duration::ZERO,
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        ),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    let call = deltas
        .iter()
        .find_map(|d| match d {
            Ok(Delta::ToolCall(c)) => Some(c),
            _ => None,
        })
        .expect("one assembled tool call");
    assert_eq!(call.id, "call_1");
    assert_eq!(call.tool, "read_file");
    assert_eq!(call.arguments, r#"{"path":"src/lib.rs"}"#);
    // The call is complete before the ending, so the loop sees it as part of the
    // turn rather than after it.
    let closed = deltas
        .iter()
        .position(|d| matches!(d, Ok(Delta::Closed { .. })))
        .expect("an ending");
    let called = deltas
        .iter()
        .position(|d| matches!(d, Ok(Delta::ToolCall(_))))
        .expect("a call");
    assert!(called < closed);
}

/// Two complete calls in two chunks, neither carrying an `index`. Defaulting a
/// missing index to zero would concatenate them into one call whose arguments are
/// two JSON objects glued together — which parses as nothing and reads as the
/// model malforming its output.
#[test]
fn two_calls_that_carry_no_index_do_not_collapse_into_one() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (
            Duration::ZERO,
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]}}]}"#,
            ),
        ),
        (
            Duration::ZERO,
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"id":"b","function":{"name":"read_file","arguments":"{\"path\":\"b\"}"}}]}}]}"#,
            ),
        ),
        (
            Duration::ZERO,
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        ),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    let calls: Vec<&ToolCall> = deltas
        .iter()
        .filter_map(|d| match d {
            Ok(Delta::ToolCall(c)) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].arguments, r#"{"path":"a"}"#);
    assert_eq!(calls[1].arguments, r#"{"path":"b"}"#);
}

/// 🚨 `finish_reason: "length"` with nothing in the payload is an **absence**,
/// and the flag that says so is set by whether any content ever arrived — not by
/// the length of a string somebody assembled afterwards.
#[test]
fn the_cap_with_an_empty_payload_is_marked_as_the_absence_it_is() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (
            Duration::ZERO,
            chunk(r#"{"choices":[{"delta":{"reasoning_content":"and on, and on"}}]}"#),
        ),
        (
            Duration::ZERO,
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#),
        ),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Commandos, &body, BUDGET));

    match deltas.last() {
        Some(Ok(Delta::Closed { finish, .. })) => assert_eq!(
            *finish,
            Finish::Length {
                content_empty: true
            }
        ),
        other => panic!("expected a capped ending, got {other:?}"),
    }
}

/// 🚨 A stream that answers without usage **stops**. `stream_options.include_usage`
/// was asked for; a turn recorded at zero tokens is a silent under-report of
/// every accounting built on top of it, and a zero from an instrument nobody
/// validated is not a measurement.
#[test]
fn a_stream_that_reports_no_usage_stops_rather_than_costing_zero_tokens() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::ZERO, text_chunk("done")),
        (Duration::ZERO, chunk(STOP)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    match only_error(&deltas) {
        ProviderError::Malformed { detail } => {
            assert!(detail.contains("include_usage"), "{detail}");
        }
        other => panic!("expected Malformed, got {other:?}"),
    }
}

/// A proxy that answers 200 and then nothing produces **no** ending, so the loop
/// names it. The provider does not invent a `Stop` that would read as a complete
/// answer.
#[test]
fn a_stream_that_ends_without_a_finish_reason_produces_no_ending() {
    let stub = Stub::new(vec![Reply::sse(vec![(
        Duration::ZERO,
        text_chunk("half an ans"),
    )])]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    assert!(
        !deltas
            .iter()
            .any(|d| matches!(d, Ok(Delta::Closed { .. }) | Err(_))),
        "{deltas:?}"
    );
    assert!(matches!(deltas.last(), Some(Ok(Delta::Text(_)))));
}

// ---------------------------------------------------------------------------
// The paths that are not the happy one
// ---------------------------------------------------------------------------

/// The donor's own fallback, inherited with its reason: a `stream: true` request
/// answered without an SSE content type is parsed whole. ⚠ On this path the
/// rung's number is a *total* budget rather than an idle gap, and the two call
/// sites look identical — which is why both carry the comment.
#[test]
fn a_reply_that_is_not_an_event_stream_is_read_as_a_whole_completion() {
    let stub = Stub::new(vec![Reply::json(
        200,
        r#"{"choices":[{"message":{"content":"answered without streaming","reasoning_content":"briefly"},"finish_reason":"stop"}],"usage":{"prompt_tokens":30,"completion_tokens":6}}"#,
    )]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    assert!(matches!(deltas.first(), Some(Ok(Delta::Opened { .. }))));
    assert!(deltas.iter().any(|d| matches!(d, Ok(Delta::Reasoning(_)))));
    assert!(
        matches!(&deltas[2], Ok(Delta::Text(t)) if t == "answered without streaming"),
        "{deltas:?}"
    );
    match deltas.last() {
        Some(Ok(Delta::Closed { usage, finish })) => {
            assert_eq!(*finish, Finish::Stop);
            assert_eq!(usage.prompt_tokens, 30);
        }
        other => panic!("expected a Closed, got {other:?}"),
    }
}

/// F84: a model swap opens a window in which the server answers 400 to a request
/// that is entirely valid. F79 prices the swap at 23.77 s round trip. One retry,
/// after 750 ms, and the second request is the same bytes as the first.
///
/// ⚠ The budget here is 2 s rather than [`BUDGET`], and the reason is a design
/// consequence worth naming: **the retry's 750 ms is spent inside the idle-gap
/// budget**, because from the consumer's side a retry is silence like any other.
/// Against the rung's 90 s that is a rounding error; against a 400 ms test
/// budget it is the whole thing.
#[test]
fn a_transient_400_during_a_model_swap_is_retried_once() {
    let stub = Stub::new(vec![
        Reply::json(400, r#"{"error":"Model is loading, please wait"}"#),
        Reply::sse(vec![
            (Duration::ZERO, text_chunk("after the swap")),
            (Duration::ZERO, chunk(STOP)),
            (Duration::ZERO, chunk(USAGE)),
            (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
        ]),
    ]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(
        &provider,
        &request(Head::Recon, &body, Duration::from_secs(2)),
    );

    assert!(
        matches!(deltas.last(), Some(Ok(Delta::Closed { .. }))),
        "{deltas:?}"
    );
    let sent = stub.requests();
    assert_eq!(sent.len(), 2, "one retry, not none and not a loop");
    assert_eq!(sent[0], sent[1], "the retry re-sent the same request");
}

/// And a 400 that is not the swap window is a refusal, reported as one. Retrying
/// a real rejection is how a bad request becomes two bad requests.
#[test]
fn a_refusal_that_is_not_the_swap_window_is_not_retried() {
    let stub = Stub::new(vec![Reply::json(
        400,
        r#"{"error":"'response_format.type' must be 'json_schema' or 'text'"}"#,
    )]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    match only_error(&deltas) {
        ProviderError::Status { code, body, .. } => {
            assert_eq!(*code, 400);
            assert!(body.contains("json_schema"), "{body}");
        }
        other => panic!("expected Status, got {other:?}"),
    }
    assert_eq!(stub.requests().len(), 1);
}

// ---------------------------------------------------------------------------
// What goes out on the wire
// ---------------------------------------------------------------------------

/// One request, read field by field. Each assertion is a finding rather than a
/// preference, and they are together because they are one request.
#[test]
fn the_request_carries_the_head_the_tools_and_the_ask_for_real_token_counts() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::ZERO, chunk(STOP)),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("fix the thing");
    drain(&provider, &request(Head::Builders, &body, BUDGET));

    let sent = &stub.requests()[0];
    let messages = sent["messages"].as_array().expect("messages");
    // The system half is the head, and it arrives only here.
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], Head::Builders.prefix());
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "fix the thing");

    // F84: real token counts are reported only when asked for.
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["stream_options"]["include_usage"], true);
    // The cap is the head's, and it is a budget whose overrun is recorded rather
    // than a limit expected to hold.
    assert_eq!(sent["max_tokens"], Head::Builders.budget());
    // ⚠ `num_ctx` has no analogue in the compat dialect and the window is fixed
    // at load time. Sending one would be the F50 trap wearing a field name.
    assert!(sent.get("num_ctx").is_none());

    // The advertised surface is the enforced one: exactly the policy's admitted
    // list, no more.
    let advertised: Vec<&str> = sent["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["function"]["name"].as_str().expect("a name"))
        .collect();
    let admitted: Vec<&str> = Head::Builders.tools().iter().map(|t| t.name).collect();
    assert_eq!(advertised, admitted);
    // And the schema on the wire is the schema in the head, parsed rather than
    // re-described.
    for spec in Head::Builders.tools() {
        let on_wire = sent["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|t| t["function"]["name"] == spec.name)
            .expect("the tool");
        let expected: Value = serde_json::from_str(spec.schema).expect("a schema");
        assert_eq!(on_wire["function"]["parameters"], expected, "{}", spec.name);
    }
}

/// A4 is one model call with no tools, so the request has no `tools` key at all —
/// not an empty array, which is a different thing to a chat template.
#[test]
fn the_judge_advertises_no_tools_at_all() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::ZERO, chunk(STOP)),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("review it");
    drain(&provider, &request(Head::Commandos, &body, BUDGET));

    assert!(stub.requests()[0].get("tools").is_none());
}

/// F82: `json_schema` with `strict: true`, and never `json_object` — the server
/// answers HTTP 400 to the older dialect, which is not a style preference.
#[test]
fn a_schema_travels_as_strict_json_schema_and_never_as_json_object() {
    const VERDICT: Schema = Schema {
        name: "verdict",
        json: r#"{"type":"object","properties":{"pass":{"type":"boolean"}},"required":["pass"],"additionalProperties":false}"#,
    };
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::ZERO, chunk(STOP)),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("judge it");
    drain(
        &provider,
        &ApiRequest {
            model: MODEL,
            head: Head::Commandos,
            body: &body,
            schema: Some(VERDICT),
            idle_gap: BUDGET,
        },
    );

    let format = &stub.requests()[0]["response_format"];
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["name"], "verdict");
    assert_eq!(format["json_schema"]["strict"], true);
    assert_eq!(format["json_schema"]["schema"]["type"], "object");
}

/// The guard on the one `expect` in the request path. Every schema in the
/// registry is inlined into a request body, so a malformed one is a panic at a
/// socket unless it is a failure here first.
#[test]
fn every_registry_schema_is_valid_json() {
    for spec in TOOLS {
        let parsed: Value = serde_json::from_str(spec.schema)
            .unwrap_or_else(|e| panic!("{}'s schema is not JSON: {e}", spec.name));
        assert_eq!(parsed["type"], "object", "{}", spec.name);
    }
}

// ---------------------------------------------------------------------------
// Through the loop
// ---------------------------------------------------------------------------

struct Fixed;

impl Tools for Fixed {
    fn run(&self, spec: &'static ToolSpec, _call: &ToolCall) -> ToolResult {
        ToolResult {
            text: format!("<output of {}>", spec.name),
            exit: Some(0),
            elapsed_ms: 1,
            unmeasured: None,
        }
    }
}

/// 🚨 **The seam's missing field, end to end.** The second request has to contain
/// the assistant turn that *asked* for the tool, carrying the call, before the
/// tool result that answers it. Without it the transcript is a reply to a
/// question that is not there: the `OpenAI` dialect rejects the message outright
/// and a lenient chat template shows the model an answer from nowhere.
#[test]
fn the_second_request_carries_the_assistant_turn_that_asked_for_the_tool() {
    let stub = Stub::new(vec![
        Reply::sse(vec![
            (
                Duration::ZERO,
                chunk(
                    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]}}]}"#,
                ),
            ),
            (
                Duration::ZERO,
                chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
            ),
            (Duration::ZERO, chunk(USAGE)),
            (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
        ]),
        Reply::sse(vec![
            (Duration::ZERO, text_chunk("it says a")),
            (Duration::ZERO, chunk(STOP)),
            (Duration::ZERO, chunk(USAGE)),
            (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
        ]),
    ]);
    let provider = stub.provider();
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("what does a say");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &Fixed, MODEL).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    match ended {
        PhaseEnded::Answered { text, report } => {
            assert_eq!(text, "it says a");
            assert_eq!(report.turns, 2);
            assert_eq!(report.tool_calls, 1);
        }
        other => panic!("expected an answer, got {other:?}"),
    }

    let second = &stub.requests()[1];
    let messages = second["messages"].as_array().expect("messages");
    let asked = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("the assistant turn that asked");
    assert_eq!(asked["tool_calls"][0]["id"], "call_1");
    assert_eq!(asked["tool_calls"][0]["type"], "function");
    assert_eq!(asked["tool_calls"][0]["function"]["name"], "read_file");
    let answered = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the tool result");
    assert_eq!(answered["tool_call_id"], "call_1");
    assert_eq!(answered["content"], "<output of read_file>");

    // And the body the loop hands back reads the same way in memory.
    let roles: Vec<Role> = body.messages().iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
    assert_eq!(body.messages()[1].tool_calls.len(), 1);
}

// ---------------------------------------------------------------------------
// Against a real server
// ---------------------------------------------------------------------------

/// The instrument's positive control, and the only test here that needs a model.
///
/// 🚨 Everything above proves this module reads *the bytes the tests write*. The
/// one thing it cannot prove is that a real server writes those bytes — in
/// particular that the reasoning trace arrives in `reasoning_content` and not
/// under some other name, which would show up as [`Delta::Reasoning`] never
/// firing and the trace signal reading `Absent` forever. That is a zero from an
/// unvalidated instrument, so it gets a positive control rather than an
/// assumption.
///
/// Start a server, then:
/// `cargo test -p abcc-engine --test http -- --ignored --nocapture`
#[test]
#[ignore = "needs a model server; the port and key change on every load"]
fn a_live_turn_reports_usage_and_a_reasoning_trace() {
    let model = std::env::var("ABCC_MODEL").unwrap_or_else(|_| MODEL.to_owned());
    let provider = OpenAiCompat::from_env().expect("build a client");
    println!("asking {} at {}", model, provider.endpoint());

    let body = Body::opening("Reply with the single word: ready.");
    let deltas = drain(
        &provider,
        &ApiRequest {
            model: &model,
            head: Head::Commandos,
            body: &body,
            schema: None,
            idle_gap: Duration::from_secs(90),
        },
    );
    for delta in &deltas {
        if let Err(e) = delta {
            panic!("the live turn failed: {e}");
        }
    }

    let ttfb = deltas
        .iter()
        .find_map(|d| match d {
            Ok(Delta::Opened { ttfb_ms }) => Some(*ttfb_ms),
            _ => None,
        })
        .expect("a first byte");
    let text: String = deltas
        .iter()
        .filter_map(|d| match d {
            Ok(Delta::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let trace: usize = deltas
        .iter()
        .filter_map(|d| match d {
            Ok(Delta::Reasoning(t)) => Some(t.len()),
            _ => None,
        })
        .sum();
    let Some(Ok(Delta::Closed { usage, finish })) = deltas.last() else {
        panic!("the live turn did not close: {deltas:?}");
    };
    println!(
        "ttfb {ttfb} ms, {} prompt + {} completion tokens, {trace} chars of trace, \
         reasoning_tokens {:?}, cached_tokens {:?}, finished {finish:?}, said {text:?}",
        usage.prompt_tokens, usage.completion_tokens, usage.reasoning_tokens, usage.cached_tokens
    );

    assert!(!text.is_empty(), "the model said nothing");
    // 🚨 The positive control. A server that reported no usage would have failed
    // the drain above; this is the other half — the counts are real numbers.
    assert!(usage.prompt_tokens > 0, "usage came back empty");
    assert!(usage.completion_tokens > 0, "usage came back empty");
    // ⚠ Not asserted: that a trace arrived. The champion is a reasoning model and
    // should produce one, but a model that does not is a legitimate answer here —
    // the number is printed so the field name can be checked against a run rather
    // than believed.
}

/// The other half of the live control, and the part no stub can settle: **does a
/// real server fragment its tool calls the way this module reassembles them?**
///
/// The index, whether an id arrives once or on every fragment, and where the
/// argument string is cut are all server behaviour. Everything above asserts
/// what the tests themselves wrote; this asserts what the server writes, through
/// the real [`TurnLoop`], with the real registry advertised in the request.
///
/// Start a server, then:
/// `cargo test -p abcc-engine --test http -- --ignored --nocapture`
#[test]
#[ignore = "needs a model server; the port and key change on every load"]
fn a_live_turn_asks_for_a_tool_and_the_call_arrives_assembled() {
    let model = std::env::var("ABCC_MODEL").unwrap_or_else(|_| MODEL.to_owned());
    let provider = OpenAiCompat::from_env().expect("build a client");
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening(
        "List the files at the top level of this repository, then say what you found.          The path you want is a single dot.",
    );
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &Fixed, &model).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let report = ended.report();
    println!(
        "{} turns, {} tool calls, {} denials, {} + {} tokens, trace {:?}, {} ms",
        report.turns,
        report.tool_calls,
        report.denials,
        report.prompt_tokens,
        report.completion_tokens,
        report.trace,
        report.elapsed_ms,
    );
    for message in body.messages() {
        println!(
            "  {:?}: {} calls, {:?}",
            message.role,
            message.tool_calls.len(),
            message.content.chars().take(90).collect::<String>()
        );
    }

    // 🚨 The claim: a real fragmented tool call reassembles into arguments that
    // parse. The donor family's one surviving hard number about output
    // reliability is a 10.5% tool-call malformation rate — and reassembling the
    // fragments wrong looks exactly like the model malforming its output, which
    // is why this is checked against a server rather than against a fixture.
    assert!(
        report.tool_calls > 0,
        "the model asked for no tools: {ended:?}"
    );
    assert_eq!(report.denials, 0, "a tool it was advertised was refused");
    let asked = body
        .messages()
        .iter()
        .find(|m| !m.tool_calls.is_empty())
        .expect("the assistant turn that asked");
    for call in &asked.tool_calls {
        let arguments: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|e| {
            panic!(
                "{} arguments did not parse: {e} in {:?}",
                call.tool, call.arguments
            )
        });
        println!("  reassembled {} {} {arguments}", call.id, call.tool);
        assert!(arguments.is_object());
    }
}

/// 🚨 **F537: the same claim as
/// `a_turn_whose_body_outlasts_the_budget_completes_while_every_gap_stays_inside_it`,
/// with the payload changed from text to tool-call arguments — and it does not
/// hold.**
///
/// Every gap on the wire is 120 ms against a 400 ms budget, so by the property
/// that test pins, this turn must complete. It does not: `OpenAiCompat::fragment`
/// folds an argument fragment into its slot and emits **no `Delta`**, and a
/// `Delta::ToolCall` exists only once `finish_reason` arrives. The consumer's
/// `recv_timeout(idle_gap)` therefore sees nothing at all while a model writes a
/// long tool call, and the hang detector fires on a stream that is delivering
/// bytes the whole time.
#[test]
fn a_turn_writing_only_tool_call_arguments_is_not_silent_and_must_not_read_as_a_hang() {
    let gap = Duration::from_millis(120);
    let mut pieces: Vec<(Duration, String)> = vec![(
        gap,
        chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"apply_patch","arguments":"{\"patch\":\""}}]}}]}"#,
        ),
    )];
    // Four more argument fragments, each one gap after the last. This is what a
    // model writing a large diff looks like on the wire.
    for i in 0..4 {
        pieces.push((
            gap,
            chunk(&format!(
                r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"function":{{"arguments":"line{i} "}}}}]}}}}]}}"#
            )),
        ));
    }
    pieces.push((
        gap,
        chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
    ));
    pieces.push((gap, chunk(USAGE)));
    pieces.push((gap, "data: [DONE]\n\n".to_owned()));

    let stub = Stub::new(vec![Reply::sse(pieces)]);
    let provider = stub.provider();
    let body = Body::opening("go");

    let started = Instant::now();
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));
    let elapsed = started.elapsed();

    assert!(
        elapsed > BUDGET,
        "the body took {elapsed:?}, which is inside the budget — the test proves nothing"
    );
    if let Some(Err(ProviderError::IdleGap { after_ms })) = deltas.last() {
        panic!(
            "the stream delivered a fragment every {gap:?} and was called idle after {after_ms} ms"
        );
    }
    let call = deltas
        .iter()
        .find_map(|d| match d {
            Ok(Delta::ToolCall(c)) => Some(c),
            _ => None,
        })
        .expect("one assembled tool call");
    assert_eq!(call.tool, "apply_patch");
}
