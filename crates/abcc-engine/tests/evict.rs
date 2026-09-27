//! B3: stale tool results stubbed before the server cuts. The first seven tests
//! are claudette's `context_evict.rs` tests, ported to abcc's `Body`; the last
//! three pin what abcc does differently and why.

use abcc_core::event::Event;
use abcc_core::redact::Secrets;
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::evict::{self, KEEP_RECENT, STUB_MARKER, stub_body};
use abcc_engine::provider::{Message, Role};
use abcc_engine::scripted::{Script, Scripted};
use abcc_engine::tools::ToolSpec;
use abcc_engine::{Body, ControlPoint, Head, Limits, ToolCall, ToolResult, Tools, TurnLoop};

const MODEL: &str = "test-model";
const ATTEMPT: AttemptId = AttemptId::at(Seq::new(7));

fn result(id: &str, text: String) -> Message {
    Message::tool_result(id, Secrets::default().scrub(text).text)
}

fn call(id: &str, tool: &str) -> Message {
    Message::assistant_calling(
        "",
        vec![ToolCall {
            id: id.to_owned(),
            tool: tool.to_owned(),
            arguments: "{}".to_owned(),
        }],
    )
}

/// A brief, then `sizes.len()` call/result pairs.
fn phase(sizes: &[usize]) -> Body {
    let mut body = Body::opening("start");
    for (i, &n) in sizes.iter().enumerate() {
        let id = format!("t{i}");
        body.append(call(&id, "read_file"));
        body.append(result(&id, "x".repeat(n)));
    }
    body
}

fn stubbed(body: &Body) -> Vec<bool> {
    body.messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.starts_with(STUB_MARKER))
        .collect()
}

#[test]
fn under_the_trigger_nothing_happens() {
    let mut body = phase(&[600; 12]);
    let window = evict::estimate_tokens("", &body) * 2;
    assert!(evict::stale(&mut body, "", window).is_none());
}

#[test]
fn the_last_k_tool_results_are_immune() {
    // Nine results; only the oldest is outside the recency window.
    let mut body = phase(&[600; 9]);
    let evicted = evict::stale(&mut body, "", 100).expect("one eviction");
    assert_eq!(evicted.results, 1);
    let mut expected = vec![false; 9];
    expected[0] = true;
    assert_eq!(stubbed(&body), expected);
}

#[test]
fn it_evicts_oldest_first_and_stops_at_the_low_water_mark() {
    // Two big stale results, eight small recent ones. Stubbing the first big
    // one is enough to fall under the low-water mark, so the second keeps its
    // body.
    let mut sizes = vec![4096, 4096];
    sizes.extend([100; KEEP_RECENT]);
    let mut body = phase(&sizes);
    let before = evict::estimate_tokens("", &body);
    // Over the trigger now; under the low water once ~1000 tokens are gone.
    let window = (before - 700) * 100 / 45;
    assert!(before * 100 >= window * evict::TRIGGER_PERCENT);

    evict::stale(&mut body, "", window).expect("an eviction");
    let s = stubbed(&body);
    assert!(s[0], "the oldest big result must be stubbed");
    assert!(!s[1], "the second big result must keep its body");
}

#[test]
fn small_results_are_skipped() {
    let mut sizes = vec![200];
    sizes.extend([600; KEEP_RECENT]);
    let mut body = phase(&sizes);
    assert!(evict::stale(&mut body, "", 100).is_none());
}

#[test]
fn an_already_stubbed_result_is_not_stubbed_again() {
    let mut body = Body::opening("start");
    let mut old = stub_body("read_file", 2048);
    old.push_str(&"x".repeat(600));
    body.append(call("t0", "read_file"));
    body.append(result("t0", old.clone()));
    for i in 1..=KEEP_RECENT {
        let id = format!("t{i}");
        body.append(call(&id, "read_file"));
        body.append(result(&id, "x".repeat(600)));
    }
    assert!(evict::stale(&mut body, "", 100).is_none());
    assert_eq!(body.messages()[2].content, old);
}

#[test]
fn the_stub_is_json_names_the_tool_and_says_do_not_re_run() {
    let body = stub_body("read_file", 1234);
    assert!(body.starts_with(STUB_MARKER));
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(parsed["evicted"], true);
    assert_eq!(parsed["tool"], "read_file");
    assert_eq!(parsed["original_chars"], 1234);
    assert!(
        parsed["note"]
            .as_str()
            .expect("a note")
            .contains("Do NOT re-run")
    );
}

#[test]
fn eviction_keeps_every_message_its_role_and_its_calls() {
    let mut body = phase(&[600; 12]);
    let shape = |b: &Body| {
        b.messages()
            .iter()
            .map(|m| (m.role, m.tool_call_id.clone(), m.tool_calls.clone()))
            .collect::<Vec<_>>()
    };
    let before = shape(&body);
    let evicted = evict::stale(&mut body, "", 100).expect("evictions");
    assert_eq!(evicted.results, 12 - KEEP_RECENT);
    assert_eq!(shape(&body), before);
}

/// abcc: one brief and then the whole phase. claudette's current-turn rule
/// would protect all of it; here the stale results still go.
#[test]
fn a_phase_with_one_brief_still_evicts_and_never_touches_the_brief() {
    let mut body = Body::opening("b".repeat(20_000));
    for i in 0..10 {
        let id = format!("t{i}");
        body.append(call(&id, "read_file"));
        body.append(result(&id, "x".repeat(600)));
    }
    let evicted = evict::stale(&mut body, "", 100).expect("evictions");
    assert_eq!(evicted.results, 2);
    assert_eq!(body.messages()[0].content, "b".repeat(20_000));
}

/// The stub names the tool that produced the result, found through its call id.
#[test]
fn the_stub_names_the_tool_that_made_the_result() {
    let mut body = Body::opening("start");
    body.append(call("g", "grep"));
    body.append(result("g", "x".repeat(600)));
    for i in 0..KEEP_RECENT {
        let id = format!("t{i}");
        body.append(call(&id, "read_file"));
        body.append(result(&id, "x".repeat(600)));
    }
    evict::stale(&mut body, "", 100).expect("an eviction");
    let parsed: serde_json::Value =
        serde_json::from_str(&body.messages()[2].content).expect("a stub");
    assert_eq!(parsed["tool"], "grep");
}

// ---------------------------------------------------------------------------
// With the re-read guard
// ---------------------------------------------------------------------------

/// Returns the call's arguments followed by 2,000 characters, so every path
/// has its own large result.
struct Files;

impl Tools for Files {
    fn run(&self, _spec: &'static ToolSpec, call: &ToolCall) -> ToolResult {
        ToolResult {
            text: format!("{}\n{}", call.arguments, "x".repeat(2000)),
            exit: Some(0),
            elapsed_ms: 1,
            unmeasured: None,
        }
    }
}

/// Reads ten files, then the first one again, and returns what the model was
/// shown for that last read.
fn the_second_read_of_a(limits: Limits) -> (String, Vec<String>) {
    let mut script: Vec<Script> = (0..10)
        .map(|i| {
            Script::calls(
                &format!("c{i}"),
                "read_file",
                &format!(r#"{{"path":"f{i}"}}"#),
            )
        })
        .collect();
    script.push(Script::calls("again", "read_file", r#"{"path":"f0"}"#));
    script.push(Script::says("done"));
    let provider = Scripted::new(script);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();

    TurnLoop::new(&provider, &Files, MODEL).limits(limits).run(
        Head::Recon,
        ATTEMPT,
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    let last = body
        .messages()
        .iter()
        .rev()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result")
        .content
        .clone();
    let notes = log
        .into_iter()
        .filter_map(|e| match e {
            Event::Note { text } if text.contains("evict:") => Some(text),
            _ => None,
        })
        .collect();
    (last, notes)
}

/// 🚨 The reason the body is stubbed rather than a wire copy. Without eviction
/// the guard withholds the second read of `f0`, because the first copy is in
/// the body. With eviction the first copy is a stub, so the guard finds no
/// earlier copy and the model gets the bytes it would otherwise have lost.
#[test]
fn a_re_read_of_an_evicted_result_is_served_whole() {
    let (control, notes) = the_second_read_of_a(Limits::default());
    assert!(
        control.starts_with("abcc: duplicate tool result"),
        "the control must show the guard withholding the re-read: {}",
        &control[..control.len().min(80)]
    );
    assert!(notes.is_empty());

    let prefix = Head::Recon.posted(abcc_engine::Tier::Exec).prefix();
    let window = evict::estimate_tokens(prefix, &Body::opening("go")) * 2;
    let (evicted, notes) = the_second_read_of_a(Limits {
        evict_window: Some(window),
        ..Limits::default()
    });
    assert!(
        evicted.starts_with(r#"{"path":"f0"}"#),
        "the re-read after eviction was not served whole: {}",
        &evicted[..evicted.len().min(80)]
    );
    assert!(!notes.is_empty(), "the eviction was not on the log");
}
