//! B3: stale tool results stubbed before the server cuts. The first seven tests
//! are claudette's `context_evict.rs` tests, ported to abcc's `Body`, at windows
//! where the stale pass alone gets under the trigger so that they test
//! claudette's rule and not abcc's emergency tier. The rest pin what abcc does
//! differently and why, the tier among them.

use abcc_core::event::Event;
use abcc_core::redact::Secrets;
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::evict::{self, KEEP_RECENT, STUB_MARKER, recent_stub_body, stub_body};
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

/// The window whose trigger this body just reaches. One stale stub gets it
/// back under, so the emergency tier never fires: claudette's rule alone.
fn at_the_trigger(body: &Body) -> usize {
    evict::estimate_tokens("", body) * 100 / evict::TRIGGER_PERCENT
}

/// Every message's role, result id and calls: what eviction must not change.
fn shape(body: &Body) -> Vec<(Role, Option<String>, Vec<ToolCall>)> {
    body.messages()
        .iter()
        .map(|m| (m.role, m.tool_call_id.clone(), m.tool_calls.clone()))
        .collect()
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
    let window = at_the_trigger(&body);
    let evicted = evict::stale(&mut body, "", window).expect("one eviction");
    assert_eq!((evicted.results, evicted.recent), (1, 0));
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

/// The recent results are small as well, or the emergency tier would take them
/// (the stale pass frees nothing here, so no window leaves the tier out).
#[test]
fn small_results_are_skipped() {
    let mut sizes = vec![200];
    sizes.extend([100; KEEP_RECENT]);
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
    // Small recent results, for the reason `small_results_are_skipped` gives.
    for i in 1..=KEEP_RECENT {
        let id = format!("t{i}");
        body.append(call(&id, "read_file"));
        body.append(result(&id, "x".repeat(100)));
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
    let before = shape(&body);
    let window = at_the_trigger(&body);
    let evicted = evict::stale(&mut body, "", window).expect("evictions");
    assert_eq!((evicted.results, evicted.recent), (12 - KEEP_RECENT, 0));
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
    let window = at_the_trigger(&body);
    let evicted = evict::stale(&mut body, "", window).expect("evictions");
    assert_eq!((evicted.results, evicted.recent), (2, 0));
    assert_eq!(body.messages()[0].content, "b".repeat(20_000));

    // Nor does the emergency tier, which takes every result but the newest.
    let evicted = evict::stale(&mut body, "", 100).expect("the tier");
    assert_eq!(evicted.recent, KEEP_RECENT - 1);
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
    let window = at_the_trigger(&body);
    evict::stale(&mut body, "", window).expect("an eviction");
    let parsed: serde_json::Value =
        serde_json::from_str(&body.messages()[2].content).expect("a stub");
    assert_eq!(parsed["tool"], "grep");
}

// ---------------------------------------------------------------------------
// abcc's emergency tier: when the stale results are not enough
// ---------------------------------------------------------------------------

/// Three big reads and nothing stale yet, the case the tier exists for: at
/// 40,960, two or three 64 KiB reads cross the trigger inside one turn.
fn three_big_reads() -> Body {
    phase(&[8000; 3])
}

#[test]
fn recent_results_go_when_the_stale_ones_are_not_enough() {
    let mut body = three_big_reads();
    let before = shape(&body);
    let evicted = evict::stale(&mut body, "", 100).expect("the tier");
    assert_eq!((evicted.results, evicted.recent), (0, 2));
    assert_eq!(stubbed(&body), [true, true, false]);
    assert_eq!(shape(&body), before, "the tier kept every message and call");
    let note = evicted.note(100);
    assert!(
        note.contains("0 stale tool result(s) and 2 recent one(s)"),
        "{note}"
    );
}

#[test]
fn the_newest_result_never_goes() {
    // Alone, and far over the window: there is nothing else to take.
    let mut body = phase(&[60_000]);
    assert!(evict::stale(&mut body, "", 100).is_none());

    // With stale ones too: the stale pass first, then every recent but the last.
    let mut body = phase(&[8000; 12]);
    let evicted = evict::stale(&mut body, "", 100).expect("both tiers");
    assert_eq!(
        (evicted.results, evicted.recent),
        (12 - KEEP_RECENT, KEEP_RECENT - 1)
    );
    let mut expected = vec![true; 12];
    expected[11] = false;
    assert_eq!(stubbed(&body), expected);
}

#[test]
fn the_tier_stops_at_the_low_water_mark() {
    let mut body = three_big_reads();
    let window = at_the_trigger(&body);
    let evicted = evict::stale(&mut body, "", window).expect("the tier");
    assert_eq!((evicted.results, evicted.recent), (0, 1));
    assert_eq!(stubbed(&body), [true, false, false]);
    assert!(evicted.after < window * evict::LOW_WATER_PERCENT / 100);
}

/// The recent stub does not say "stale" or "do not re-run": the model may not
/// have read the result yet, and the way back is a smaller part.
#[test]
fn the_recent_stub_is_marked_names_the_tool_and_points_at_a_smaller_read() {
    let mut body = three_big_reads();
    evict::stale(&mut body, "", 100).expect("the tier");
    assert_eq!(
        body.messages()[2].content,
        recent_stub_body("read_file", 8000)
    );

    let stub = recent_stub_body("read_file", 8000);
    assert!(stub.starts_with(STUB_MARKER));
    let parsed: serde_json::Value = serde_json::from_str(&stub).expect("valid JSON");
    assert_eq!(parsed["evicted"], true);
    assert_eq!(parsed["tool"], "read_file");
    assert_eq!(parsed["original_chars"], 8000);
    let note = parsed["note"].as_str().expect("a note");
    assert!(note.contains("from_line..to_line"), "{note}");
    assert!(
        !note.contains("Stale") && !note.contains("Do NOT re-run"),
        "{note}"
    );
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

// ---------------------------------------------------------------------------
// With the F748 detector
// ---------------------------------------------------------------------------

/// What the brief alone is estimated at, under the head these tests post.
fn base() -> u32 {
    let prefix = Head::Recon.posted(abcc_engine::Tier::Exec).prefix();
    u32::try_from(evict::estimate_tokens(prefix, &Body::opening("go"))).expect("small")
}

/// Three reads and an answer, the server reporting `base() + 10` and
/// `base() + 520` for the first two calls and `third`, `third + 100` for the
/// last two. Evicting, the window is over the trigger after two ~500-token
/// results and not after one, so an eviction comes before each of the last two
/// calls. The `PromptCut`s logged, as `(reported, high_water)`, and whether
/// anything was evicted.
fn cuts_over_three_reads(evicting: bool, third: u32) -> (Vec<(u32, u32)>, bool) {
    let base = base();
    let provider = Scripted::new(vec![
        Script::calls("c0", "read_file", r#"{"path":"f0"}"#).with_prompt_tokens(base + 10),
        Script::calls("c1", "read_file", r#"{"path":"f1"}"#).with_prompt_tokens(base + 520),
        Script::calls("c2", "read_file", r#"{"path":"f2"}"#).with_prompt_tokens(third),
        Script::says("done").with_prompt_tokens(third + 100),
    ]);
    let evict_window = evicting.then(|| (base as usize + 770) * 100 / evict::TRIGGER_PERCENT);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("go");
    let mut log: Vec<Event> = Vec::new();
    TurnLoop::new(&provider, &Files, MODEL)
        .limits(Limits {
            evict_window,
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
    let cuts = log
        .iter()
        .filter_map(|e| match e {
            Event::PromptCut {
                reported,
                high_water,
                ..
            } => Some((*reported, *high_water)),
            _ => None,
        })
        .collect();
    let evicted = log
        .iter()
        .any(|e| matches!(e, Event::Note { text } if text.contains("evict:")));
    (cuts, evicted)
}

/// 🚨 Eviction shrinks the body on purpose, so a smaller prompt after one is not
/// the server's cut. Found by the first live smoke of the emergency tier: an
/// eviction took the prompt from 16,816 to 15,998 tokens and the log said the
/// server had cut it. The control runs the same script without eviction, where
/// the fall is a cut and has to be reported, so the detector is live here.
#[test]
fn an_eviction_is_not_a_cut() {
    let third = base() + 300;
    let (cuts, evicted) = cuts_over_three_reads(false, third);
    assert!(!evicted);
    let high = base() + 520;
    assert_eq!(cuts, [(third, high), (third + 100, high)], "the control");

    let (cuts, evicted) = cuts_over_three_reads(true, third);
    assert!(evicted, "nothing was evicted, so this proves nothing");
    assert!(cuts.is_empty(), "an eviction was logged as a cut: {cuts:?}");
}

/// 🚨 And a cut right after an eviction is still a cut. Resetting the high water
/// to zero at an eviction was tried and hid one on the live control: the stale
/// pass left an estimated 50,881 tokens and the server reported 27,355, about
/// half, which is what a cut looks like (F745).
#[test]
fn a_cut_right_after_an_eviction_is_still_reported() {
    let half = base() / 2;
    let (cuts, evicted) = cuts_over_three_reads(true, half);
    assert!(evicted);
    assert_eq!(
        cuts.iter()
            .map(|&(reported, _)| reported)
            .collect::<Vec<_>>(),
        [half, half + 100],
        "{cuts:?}"
    );
}
