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
use abcc_engine::openai::{self, OpenAiCompat};
use abcc_engine::provider::{ApiRequest, Delta, ProviderError, Role, Schema, Temperature};
use abcc_engine::tools::{TOOLS, ToolSpec};
use abcc_engine::{
    Body, ControlPoint, Head, Limits, PhaseEnded, Provider, Tier, ToolCall, ToolResult, Tools,
    TurnLoop,
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
        // An uncapped slot, which is what every case here is about: the cap's
        // own effect on the wire is `a_capped_slot_narrows_the_wire_tool_array`
        // below.
        posting: head.posted(Tier::Exec),
        model: MODEL,
        body,
        schema: None,
        idle_gap,
        // The cases here are about the ordinary read budget. F625's second gap
        // has its own tests, which set it deliberately.
        tool_call_gap: idle_gap,
        liveness_slice: idle_gap,
        seed: 1234,
        temperature: Temperature::default(),
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

/// 🧪 F841 probe (a): a `stop` turn with an empty payload whose trace **ends**
/// in `<tool_call>` blocks asks for those calls — F839's `said_nothing` shape.
/// A block the trace moved past is not a request, and a turn that said
/// something keeps its words.
#[test]
fn a_tool_call_written_inside_the_trace_is_recovered_only_from_its_end() {
    let trace = "Plan: maybe\n<tool_call>\n<function=search>\n<parameter=pattern>\nx\n\
                 </parameter>\n</function>\n</tool_call>\nNo, read them instead.\n\n\
                 <tool_call>\n<function=read_file>\n<parameter=path>\njobs/a.py\n</parameter>\n\
                 </function>\n</tool_call>\n<tool_call>\n<function=read_file>\n\
                 <parameter=path>\n2024\n</parameter>\n<parameter=offset>\n10\n</parameter>\n\
                 </function>\n</tool_call>\n";
    let reasoning = serde_json::json!({"choices":[{"delta":{"reasoning_content": trace}}]});
    let turn = |extra: Vec<(Duration, String)>| {
        let mut chunks = vec![(Duration::ZERO, chunk(&reasoning.to_string()))];
        chunks.extend(extra);
        chunks.extend([
            (Duration::ZERO, chunk(STOP)),
            (Duration::ZERO, chunk(USAGE)),
            (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
        ]);
        let stub = Stub::new(vec![Reply::sse(chunks)]);
        let body = Body::opening("go");
        drain(&stub.provider(), &request(Head::Commandos, &body, BUDGET))
    };
    let calls = |deltas: &[Result<Delta, ProviderError>]| -> Vec<ToolCall> {
        deltas
            .iter()
            .filter_map(|d| match d {
                Ok(Delta::ToolCall(c)) => Some(c.clone()),
                _ => None,
            })
            .collect()
    };

    let silent = calls(&turn(vec![]));
    assert_eq!(silent.len(), 2, "{silent:?}");
    assert_eq!(silent[0].tool, "read_file");
    assert_eq!(silent[0].arguments, r#"{"path":"jobs/a.py"}"#);
    assert_eq!(silent[0].id, "from_reasoning_0");
    // A number-shaped path would be mangled by a blind JSON read; the probe
    // accepts that (the server's parser does the same without a schema).
    assert_eq!(silent[1].arguments, r#"{"offset":10,"path":2024}"#);

    let spoke = calls(&turn(vec![(Duration::ZERO, text_chunk("the answer"))]));
    assert!(spoke.is_empty(), "{spoke:?}");
}

/// F849: a `stop` turn whose reply **ends** in `<tool_call>` blocks asks for
/// those calls. The first reply is `Q39`'s, byte for byte: no newline after
/// `<tool_call>`, which is why the server returned no call. Words before the
/// run stay words; a block the reply moved past is not a request.
#[test]
fn a_tool_call_written_as_the_reply_is_recovered_only_from_its_end() {
    const Q39: &str = "<tool_call><function=read_file>\n<parameter=path>\nsolution.ts\n\
                       </parameter>\n</function>\n</tool_call>";
    let turn = |reply: &str| {
        let text = serde_json::json!({"choices":[{"delta":{"content": reply}}]});
        let stub = Stub::new(vec![Reply::sse(vec![
            (Duration::ZERO, chunk(&text.to_string())),
            (Duration::ZERO, chunk(STOP)),
            (Duration::ZERO, chunk(USAGE)),
            (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
        ])]);
        let body = Body::opening("go");
        drain(&stub.provider(), &request(Head::Builders, &body, BUDGET))
            .into_iter()
            .filter_map(|d| match d {
                Ok(Delta::ToolCall(c)) => Some(c),
                _ => None,
            })
            .collect::<Vec<ToolCall>>()
    };

    let alone = turn(Q39);
    assert_eq!(alone.len(), 1, "{alone:?}");
    assert_eq!(alone[0].id, "from_reply_0");
    assert_eq!(alone[0].tool, "read_file");
    assert_eq!(alone[0].arguments, r#"{"path":"solution.ts"}"#);
    assert_eq!(openai::reply_before_calls(Q39), "");

    let said = format!("Let me look at the file first.\n\n{Q39}\n");
    assert_eq!(turn(&said).len(), 1);
    assert_eq!(
        openai::reply_before_calls(&said),
        "Let me look at the file first."
    );

    let moved_on = format!("{Q39}\nActually, the fix is on line 2.");
    assert!(turn(&moved_on).is_empty());
    assert_eq!(openai::reply_before_calls(&moved_on), moved_on);
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

/// 🚨🚨 **F752: the window refusal is not a fault in this engine, and it used to
/// be recorded as one.**
///
/// The body is this server's, verbatim off the wire (measured 2026-09-13 at two
/// sizes). ⚠ **Note the nesting** — the outer `error` is a *string* holding a
/// second JSON document, so `body["error"]["type"]` reads nothing and the naive
/// structured parse finds no fields at all. That is the whole reason the scan is
/// a substring search.
///
/// Three claims, and the third is the one with a cost attached: the numbers are
/// read rather than estimated, the `Why` is an **absence** and not a
/// `HardFailure`, and the request is **not retried** — re-sending a prompt that
/// does not fit produces the identical refusal.
#[test]
fn a_window_refusal_is_read_as_an_overflow_and_not_as_an_engine_fault() {
    let stub = Stub::new(vec![Reply::json(
        400,
        concat!(
            r#"{"error":"Engine protocol predict request returned 400: "#,
            r#"{\"error\":{\"code\":400,\"message\":\"request (44477 tokens) exceeds the "#,
            r#"available context size (40960 tokens), try increasing it\","#,
            r#"\"type\":\"exceed_context_size_error\",\"n_prompt_tokens\":44477,"#,
            r#"\"n_ctx\":40960}}"}"#
        ),
    )]);
    let provider = stub.provider();
    let body = Body::opening("go");
    let deltas = drain(&provider, &request(Head::Recon, &body, BUDGET));

    let error = only_error(&deltas);
    match error {
        ProviderError::ContextOverflow {
            window,
            prompt_tokens,
        } => {
            assert_eq!(*window, 40_960);
            assert_eq!(*prompt_tokens, 44_477);
        }
        other => panic!("expected ContextOverflow, got {other:?}"),
    }
    assert_eq!(
        error.why(),
        Why::ContextOverflow {
            window: 40_960,
            prompt_tokens: 44_477
        },
        "🚨 an overflow recorded as EngineError is a HardFailure, which asserts \
         another attempt would repeat this unchanged -- the one thing that is not \
         true of it (F496)"
    );
    assert_eq!(
        stub.requests().len(),
        1,
        "a prompt that does not fit was re-sent"
    );
}

/// The control for the classifier, and it is not a formality: a 400 that is not
/// the window must keep today's classification. **Both numbers or nothing** --
/// inventing a window for a body whose fields have been renamed would put a
/// fabricated measurement on the log, which is worse than the wrong class.
#[test]
fn a_400_that_is_not_the_window_is_still_a_status() {
    for body in [
        r#"{"error":"'response_format.type' must be 'json_schema' or 'text'"}"#,
        // The marker with no numbers beside it: the shape a server rename
        // produces.
        r#"{"error":{"type":"exceed_context_size_error","message":"too big"}}"#,
    ] {
        let stub = Stub::new(vec![Reply::json(400, body)]);
        let provider = stub.provider();
        let opening = Body::opening("go");
        let deltas = drain(&provider, &request(Head::Recon, &opening, BUDGET));
        assert!(
            matches!(only_error(&deltas), ProviderError::Status { code: 400, .. }),
            "{body} was reclassified: {:?}",
            only_error(&deltas)
        );
    }
}

// ---------------------------------------------------------------------------
// What goes out on the wire
// ---------------------------------------------------------------------------

/// One request, read field by field. Each assertion is a finding rather than a
/// preference, and they are together because they are one request.
#[test]
fn a_server_temperature_sends_none_so_the_old_arm_can_still_be_flown() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::ZERO, chunk(STOP)),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("fix the thing");
    let req = ApiRequest {
        temperature: Temperature::Server,
        ..request(Head::Builders, &body, BUDGET)
    };
    drain(&provider, &req);

    let sent = &stub.requests()[0];
    assert!(sent.get("temperature").is_none());
    assert_eq!(sent["seed"], 1234);
}

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
    assert_eq!(
        messages[0]["content"],
        Head::Builders.posted(Tier::Exec).prefix()
    );
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
    // 🚨 F715: the seed. Measured on the champion, five identical requests
    // without one are five distinct answers and with one are a single answer.
    // ⚠ A JSON *number*, not a string: LM Studio parses this in TypeScript.
    assert_eq!(sent["seed"], 1234);
    assert!(sent["seed"].is_number());
    // 🚨 Temperature 0 by default since 2026-09-27 (David's ruling after F840,
    // reversing 2026-09-12's *send none*). A number measured before that date
    // was taken at the server's temperature and is not comparable to one after.
    assert_eq!(sent["temperature"], 0);
    assert!(sent.get("top_p").is_none());

    // The advertised surface is the enforced one: exactly the policy's admitted
    // list, no more.
    let advertised: Vec<&str> = sent["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["function"]["name"].as_str().expect("a name"))
        .collect();
    let admitted: Vec<&str> = Head::Builders
        .posted(Tier::Exec)
        .tools()
        .iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(advertised, admitted);
    // And the schema on the wire is the schema in the head, parsed rather than
    // re-described.
    for spec in Head::Builders.posted(Tier::Exec).tools() {
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
            posting: Head::Commandos.posted(Tier::Exec),
            body: &body,
            schema: Some(VERDICT),
            idle_gap: BUDGET,
            tool_call_gap: BUDGET,
            liveness_slice: BUDGET,
            seed: 1234,
            temperature: Temperature::default(),
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
            posting: Head::Commandos.posted(Tier::Exec),
            body: &body,
            schema: None,
            idle_gap: Duration::from_secs(90),
            tool_call_gap: Limits::default().tool_call_gap,
            liveness_slice: Duration::from_secs(10),
            seed: 1234,
            temperature: Temperature::default(),
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

// ---------------------------------------------------------------------------
// 🚨 F625 — the two budgets, and F592's slices
// ---------------------------------------------------------------------------

/// A request with the two gaps set apart, which is the only way to see F625.
fn request_with_gaps(
    head: Head,
    body: &Body,
    idle_gap: Duration,
    tool_call_gap: Duration,
    slice: Duration,
) -> ApiRequest<'_> {
    ApiRequest {
        posting: head.posted(Tier::Exec),
        model: MODEL,
        body,
        schema: None,
        idle_gap,
        tool_call_gap,
        liveness_slice: slice,
        seed: 1234,
        temperature: Temperature::default(),
    }
}

/// The wire shape the raw capture found, scaled down: the call is **announced**
/// with an empty argument string, then nothing, then the whole thing at once.
///
/// 🚨 This is not an invented shape. `research/spikes/f606-sse/bigarg.sse.jsonl`
/// is exactly this: `name: "apply_patch", arguments: ""` at t=15,171 ms, then
/// **211.2 s of nothing**, then a single fragment carrying all 33,962 characters
/// at t=226,377 ms.
fn buffered_call(silence: Duration) -> Vec<(Duration, String)> {
    vec![
        (
            Duration::ZERO,
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"apply_patch","arguments":""}}]}}]}"#,
            ),
        ),
        (
            silence,
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"type":"function","function":{"arguments":"{\"patch\":\"a big diff\"}"}}]}}]}"#,
            ),
        ),
        (
            Duration::ZERO,
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        ),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ]
}

/// 🚨 **F625. The silence while a tool call is being written is not a hang, and
/// the announcement is what makes the difference observable.**
///
/// The gap here is three times `idle_gap` and half `tool_call_gap` — which is
/// the real proportion in miniature: a successful 33,962-character `apply_patch`
/// was 211.2 s of unbroken silence against a 90 s per-read budget. Before this,
/// that call died and the record said `Why::Timeout` about work that was about
/// to land.
#[test]
fn the_silence_while_a_tool_call_is_written_is_not_a_hang() {
    let stub = Stub::new(vec![Reply::sse(buffered_call(Duration::from_millis(600)))]);
    let provider = stub.provider();
    let body = Body::opening("write a big patch");

    let deltas = drain(
        &provider,
        &request_with_gaps(
            Head::Builders,
            &body,
            Duration::from_millis(200),
            Duration::from_millis(1200),
            Duration::from_millis(50),
        ),
    );

    assert!(
        !deltas.iter().any(std::result::Result::is_err),
        "a call that was going to arrive was killed: {deltas:?}"
    );
    let opened: Vec<&str> = deltas
        .iter()
        .filter_map(|d| match d {
            Ok(Delta::ToolCallOpened { tool }) => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(opened, ["apply_patch"], "the announcement is the signal");
    let calls: Vec<&str> = deltas
        .iter()
        .filter_map(|d| match d {
            Ok(Delta::ToolCall(c)) => Some(c.tool.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(calls, ["apply_patch"], "and the call itself arrived");
}

/// ⚠ **The wider budget is still a stop.** F625 moves the ceiling; it does not
/// remove it, and a call that outlasts `tool_call_gap` is an `IdleGap` reporting
/// **that** budget rather than the ordinary one — otherwise the record would
/// name a number the read was never spent against.
#[test]
fn a_tool_call_that_outlasts_its_own_budget_is_still_a_hang() {
    let stub = Stub::new(vec![Reply::sse(buffered_call(Duration::from_secs(30)))]);
    let provider = stub.provider();
    let body = Body::opening("write a big patch");

    let deltas = drain(
        &provider,
        &request_with_gaps(
            Head::Builders,
            &body,
            Duration::from_millis(100),
            Duration::from_millis(400),
            Duration::from_millis(50),
        ),
    );

    match only_error(&deltas) {
        ProviderError::IdleGap { after_ms } => assert_eq!(
            *after_ms, 400,
            "the record has to name the budget the read was spent against"
        ),
        other => panic!("expected IdleGap, got {other:?}"),
    }
}

/// 🚨 **F592. A silence detector that rides the data path cannot observe silence
/// — unless the wait is taken in slices.**
///
/// The stream says nothing for six slices. Before this the consumer blocked in
/// one `recv_timeout(idle_gap)` and the turn loop's liveness check, which sits at
/// the top of that loop, could not run — which is why the field log carries 25
/// `model_call_started` → `liveness_mark` gaps of up to 73 s with nothing between
/// them.
#[test]
fn a_quiet_stream_still_wakes_the_reader_often_enough_to_mark_the_log() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::from_millis(300), text_chunk("finally")),
        (Duration::ZERO, chunk(STOP)),
        (Duration::ZERO, chunk(USAGE)),
        (Duration::ZERO, "data: [DONE]\n\n".to_owned()),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");

    let deltas = drain(
        &provider,
        &request_with_gaps(
            Head::Recon,
            &body,
            Duration::from_millis(900),
            Duration::from_millis(900),
            Duration::from_millis(50),
        ),
    );

    let waits: Vec<u64> = deltas
        .iter()
        .filter_map(|d| match d {
            Ok(Delta::Waiting { silent_ms }) => Some(*silent_ms),
            _ => None,
        })
        .collect();
    assert!(
        waits.len() >= 3,
        "a 300 ms silence at a 50 ms slice has to wake the reader repeatedly, got {waits:?}"
    );
    // 🚨 The silence accumulates across slices rather than restarting at each
    // one. This is the defect the first draft of the fix had: a per-call clock
    // would report the slice every time and the budget would never be spent.
    assert!(
        waits.windows(2).all(|w| w[1] > w[0]),
        "the reported silence has to grow: {waits:?}"
    );
    assert!(
        !deltas.iter().any(std::result::Result::is_err),
        "and none of it is a failure: {deltas:?}"
    );
}

/// 🚨 **Slicing the wait must not extend the budget**, which is the whole risk of
/// the F592 repair: a clock restarted on every slice turns a 90 s gap into a
/// forever. The gap here is far past the budget and the budget still ends it, at
/// the budget, naming the budget.
#[test]
fn taking_the_wait_in_slices_does_not_lengthen_the_budget() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (Duration::ZERO, text_chunk("half an ans")),
        (Duration::from_secs(30), chunk(STOP)),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("go");

    let started = Instant::now();
    let deltas = drain(
        &provider,
        &request_with_gaps(
            Head::Recon,
            &body,
            BUDGET,
            BUDGET,
            Duration::from_millis(20),
        ),
    );
    let elapsed = started.elapsed();

    match only_error(&deltas) {
        ProviderError::IdleGap { after_ms } => assert_eq!(*after_ms, 400),
        other => panic!("expected IdleGap, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "twenty slices of 20 ms must still end at the 400 ms budget, took {elapsed:?}"
    );
}

/// 🚨 **The wide budget applies inside a tool call and nowhere else.**
///
/// The counting is what enforces that, and getting it wrong is silent: if opens
/// are never matched against deliveries, the first tool call of a turn widens the
/// gap for the whole rest of it, and a genuinely dead socket after a successful
/// call waits `tool_call_gap` instead of `idle_gap`. Nothing about the turn looks
/// different — it just takes three times as long to notice.
///
/// Here the call opens, is delivered, and *then* the stream dies. That silence is
/// an ordinary one and has to be spent against the ordinary budget.
#[test]
fn the_wider_budget_stops_applying_once_the_call_has_arrived() {
    let stub = Stub::new(vec![Reply::sse(vec![
        (
            Duration::ZERO,
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"apply_patch","arguments":""}}]}}]}"#,
            ),
        ),
        (
            Duration::from_millis(300),
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"patch\":\"p\"}"}}]}}]}"#,
            ),
        ),
        // Delivered. Now the socket goes quiet for far longer than `idle_gap`,
        // and this is no longer a call being written.
        (Duration::from_secs(30), chunk(STOP)),
    ])]);
    let provider = stub.provider();
    let body = Body::opening("write a patch then stall");

    let started = Instant::now();
    let deltas = drain(
        &provider,
        &request_with_gaps(
            Head::Builders,
            &body,
            Duration::from_millis(200),
            Duration::from_millis(1500),
            Duration::from_millis(50),
        ),
    );
    let elapsed = started.elapsed();

    match only_error(&deltas) {
        ProviderError::IdleGap { after_ms } => assert_eq!(
            *after_ms, 200,
            "the silence after a delivered call is an ordinary one"
        ),
        other => panic!("expected IdleGap, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_millis(1500),
        "it waited the tool-call budget for a silence that was not one: {elapsed:?}"
    );
}

/// 🚨 **F625, end to end against the real server: the call that used to die.**
///
/// This is `research/tools/ssecapture.py`'s big-argument arm, driven through
/// `OpenAiCompat` instead of through a raw socket — the same prompt, the same
/// tool, the same model. The capture that produced F625 measured **211.2 s of
/// unbroken silence** between the announcement and the call, against a 90 s
/// per-read budget: the arguments arrived in the last 4 ms of a 226-second call.
/// Under the old single budget this turn was `ProviderError::IdleGap` and the
/// attempt was recorded `Why::Timeout` for work that was about to land.
///
/// It asserts three things and prints the fourth:
///
/// 1. the call **arrives**, which is the fix;
/// 2. the announcement arrives **long before** it, which is the signal the fix
///    rests on — if the server ever stops buffering, this is where we find out;
/// 3. the wait produced [`Delta::Waiting`] deltas, which is F592: without them
///    the turn loop cannot mark a silence it is sitting inside.
///
/// ⚠ `#[ignore]`d, like every `*_live_*` test here: it needs a model server, and
/// it costs minutes rather than milliseconds.
#[test]
#[ignore = "needs a model server; the port and key change on every load"]
fn a_live_turn_writing_a_large_tool_call_survives_its_own_silence() {
    let model = std::env::var("ABCC_MODEL").unwrap_or_else(|_| MODEL.to_owned());
    let provider = OpenAiCompat::from_env().expect("build a client");

    // ssecapture.py's `USER` arm, verbatim: one prompt that forces a long
    // `diff` argument out of the builders head's shape.
    let body = Body::opening(
        "Create a new file `metrics.py` holding a complete Python module for a streaming \
         statistics accumulator. It must implement, with full docstrings and type hints: a \
         Welford mean/variance accumulator, a P-square quantile estimator for p50/p90/p95/p99, \
         an exponentially weighted moving average, a reservoir sampler, a bounded histogram \
         with configurable bucket edges, and a `Summary` dataclass that renders all of them as \
         an aligned table. Include a `__main__` block that demonstrates every class on \
         synthetic data. Write it in one `apply_patch` call as a unified diff creating the \
         file. Do not abbreviate and do not elide any function body.",
    );

    let started = Instant::now();
    let mut announced_at = None;
    let mut waits = 0usize;
    let mut longest_quiet = 0u64;
    let mut arrived = None;

    let req = ApiRequest {
        model: &model,
        posting: Head::Builders.posted(Tier::Exec),
        body: &body,
        schema: None,
        idle_gap: Duration::from_secs(90),
        tool_call_gap: Limits::default().tool_call_gap,
        liveness_slice: Duration::from_secs(10),
        seed: 1234,
        temperature: Temperature::default(),
    };
    let mut stream = provider.start(&req).expect("open the turn");
    while let Some(delta) = stream.next_delta() {
        match delta.expect("the live turn failed") {
            Delta::ToolCallOpened { tool } => {
                announced_at = Some(started.elapsed());
                println!("  announced {tool} at {:?}", started.elapsed());
            }
            Delta::Waiting { silent_ms } => {
                waits += 1;
                longest_quiet = longest_quiet.max(silent_ms);
            }
            Delta::ToolCall(call) => {
                println!(
                    "  {} arrived at {:?}, {} chars",
                    call.tool,
                    started.elapsed(),
                    call.arguments.len()
                );
                arrived = Some(call);
            }
            _ => {}
        }
    }

    let call = arrived.expect("the tool call never arrived — F625 is not fixed");
    let announced = announced_at.expect("the call was never announced");
    let quiet = started.elapsed().saturating_sub(announced);
    println!(
        "  announcement -> call: {quiet:?}; {waits} Waiting deltas, longest quiet {longest_quiet} ms"
    );

    // ⚠ Either write-class tool is a pass. The first run of this asserted
    // `apply_patch` and the model chose `write_file`, which is *correct* — the
    // task creates a file rather than editing one, and the Builders head offers
    // both. `ssecapture.py` only ever offered `apply_patch`, so this is the first
    // time the real tool set has been in front of this prompt.
    assert!(
        matches!(call.tool.as_str(), "apply_patch" | "write_file"),
        "expected a write-class tool, got {}",
        call.tool
    );
    assert!(
        call.arguments.len() > 2_000,
        "this arm is meant to force a large argument, got {} chars",
        call.arguments.len()
    );
    // 🚨 The premise, re-checked every run rather than remembered: this server
    // buffers, so the announcement precedes the call by most of the turn. If
    // this ever fails, LM Studio has started streaming arguments and F537's
    // instrument becomes the right one again.
    assert!(
        quiet > Duration::from_secs(5),
        "the server no longer buffers tool calls — re-read F622 before trusting F625's fix"
    );
    // 🚨 F592: the silence was observable while it was happening.
    assert!(
        waits > 0,
        "no Waiting delta arrived, so nothing could have marked the log during the silence"
    );
}
