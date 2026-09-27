//! Chat compaction: a long conversation summarised before the window fills. The
//! first eight tests are claudette's `compact.rs` and `compaction_policy.rs`
//! tests, ported to abcc's `Body`; the rest pin what abcc does differently.

use abcc_core::redact::Secrets;
use abcc_engine::compact::{
    self, CEILING_TOKENS, FLOOR_TOKENS, PREAMBLE, PRESERVE_RECENT, continuation, format_summary,
    is_summary, threshold,
};
use abcc_engine::evict::{self, stub_body};
use abcc_engine::provider::{Message, Role};
use abcc_engine::{Body, ToolCall};

fn result(id: &str, text: String) -> Message {
    Message::tool_result(id, Secrets::default().scrub(text).text)
}

fn call(id: &str, tool: &str, arguments: &str) -> Message {
    Message::assistant_calling(
        "",
        vec![ToolCall {
            id: id.to_owned(),
            tool: tool.to_owned(),
            arguments: arguments.to_owned(),
        }],
    )
}

/// A brief, then `turns` operator lines each answered by one read and a reply.
fn conversation(turns: usize, size: usize) -> Body {
    let mut body = Body::opening("## The task\n\nadd two()");
    for i in 0..turns {
        let id = format!("t{i}");
        body.append(Message::user(format!("ask {i}: {}", "u".repeat(size))));
        body.append(call(&id, "read_file", r#"{"path":"src/lib.rs"}"#));
        body.append(result(&id, "r".repeat(size)));
        body.append(Message::assistant(format!(
            "reply {i}: {}",
            "a".repeat(size)
        )));
    }
    body
}

/// Every tool result has its call earlier in the body.
fn no_orphans(body: &Body) -> bool {
    let messages = body.messages();
    messages.iter().enumerate().all(|(i, m)| {
        m.role != Role::Tool
            || messages[..i].iter().any(|earlier| {
                earlier
                    .tool_calls
                    .iter()
                    .any(|c| Some(c.id.as_str()) == m.tool_call_id.as_deref())
            })
    })
}

// ---------------------------------------------------------------------------
// claudette's
// ---------------------------------------------------------------------------

#[test]
fn formats_compact_summary_like_upstream() {
    let summary = "<analysis>scratch</analysis>\n<summary>Kept work</summary>";
    assert_eq!(format_summary(summary), "Summary:\nKept work");
}

#[test]
fn continuation_message_warns_against_assuming_completion() {
    let msg = continuation("<summary>work in progress</summary>", false, true);
    assert!(
        msg.contains("re-check the current state with a tool"),
        "continuation must tell the model to verify before claiming done: {msg}"
    );
    assert!(msg.contains("Do NOT assume"), "got: {msg}");
    assert!(msg.starts_with(PREAMBLE), "got: {msg}");
}

#[test]
fn leaves_small_sessions_unchanged() {
    let mut body = conversation(1, 10);
    let before = body.clone();
    assert!(compact::compact(&mut body, "", 1).is_none());
    assert_eq!(body, before);
}

#[test]
fn compacts_older_messages_into_a_summary() {
    let mut body = conversation(8, 800);
    let before = evict::estimate_tokens("", &body);
    let compacted = compact::compact(&mut body, "", 1).expect("a compaction");

    // 33 messages: the brief, 20 summarised, the last 12 kept.
    assert_eq!(compacted.removed, 33 - 1 - PRESERVE_RECENT);
    assert_eq!(body.len(), 1 + 1 + PRESERVE_RECENT);
    let summary = &body.messages()[1];
    assert!(is_summary(summary), "{}", summary.content);
    assert!(summary.content.contains("Summary:"));
    assert!(summary.content.contains("Scope:"));
    assert!(summary.content.contains("Key timeline:"));
    assert_eq!(compacted.before, before);
    assert!(compacted.after < before, "{compacted:?}");
    assert_eq!(compacted.after, evict::estimate_tokens("", &body));
}

#[test]
fn truncates_long_blocks_in_summary() {
    let mut body = conversation(8, 800);
    compact::compact(&mut body, "", 1).expect("a compaction");
    let summary = &body.messages()[1].content;
    let line = summary
        .lines()
        .find(|l| l.starts_with("  - user: ask 0"))
        .expect("the first ask in the timeline");
    let entry = line.trim_start_matches("  - user: ");
    assert!(entry.ends_with('…'), "{line}");
    assert!(entry.chars().count() <= 161, "{line}");
}

#[test]
fn extracts_key_files_from_message_content() {
    let mut body = Body::opening("## The task");
    body.append(Message::user(
        "Update rust/crates/runtime/src/compact.rs and rust/crates/runtime/src/main.rs next.",
    ));
    for m in &conversation(8, 800).messages()[1..] {
        body.append(m.clone());
    }
    compact::compact(&mut body, "", 1).expect("a compaction");
    let summary = &body.messages()[1].content;
    assert!(
        summary.contains("rust/crates/runtime/src/compact.rs"),
        "{summary}"
    );
    assert!(
        summary.contains("rust/crates/runtime/src/main.rs"),
        "{summary}"
    );
}

#[test]
fn infers_pending_work_from_recent_messages() {
    let mut body = Body::opening("## The task");
    for i in 0..6 {
        body.append(Message::user(format!("old {i} {}", "x".repeat(2_000))));
    }
    body.append(Message::user("done"));
    body.append(Message::assistant(
        "Next: update tests and follow up on remaining CLI polish.",
    ));
    for i in 0..PRESERVE_RECENT {
        body.append(Message::user(format!("later {i} {}", "x".repeat(2_000))));
    }
    compact::compact(&mut body, "", 1).expect("a compaction");
    let summary = &body.messages()[1].content;
    let pending = summary
        .split("- Pending work:\n")
        .nth(1)
        .expect("a pending-work section");
    assert!(pending.starts_with("  - Next: update tests"), "{summary}");
}

/// `compaction_policy.rs`: half the window, clamped.
#[test]
fn the_threshold_is_half_the_window_clamped() {
    assert_eq!(threshold(32_768), 16_384);
    assert_eq!(threshold(40_960), 20_480);
    assert_eq!(threshold(1_000), FLOOR_TOKENS);
    assert_eq!(threshold(8_000_000), CEILING_TOKENS);
}

// ---------------------------------------------------------------------------
// abcc's
// ---------------------------------------------------------------------------

#[test]
fn under_the_threshold_nothing_happens() {
    let mut body = conversation(8, 800);
    let window = evict::estimate_tokens("", &body) * 2 + 2;
    assert!(compact::compact(&mut body, "", window).is_none());
    // And the head's prefix counts toward it.
    let prefix = "p".repeat(40_000);
    assert!(compact::compact(&mut body, &prefix, window).is_some());
}

/// The brief is the task. It is message 0 before and after, byte for byte.
#[test]
fn the_brief_is_never_summarised() {
    let mut body = conversation(8, 800);
    let brief = body.messages()[0].clone();
    compact::compact(&mut body, "", 1).expect("a compaction");
    assert_eq!(body.messages()[0], brief);
    assert!(!body.messages()[1].content.contains("## The task"));
}

/// The last messages are kept word for word, and the kept part never starts
/// with a result whose call went into the summary.
#[test]
fn the_kept_part_is_verbatim_and_has_no_orphaned_result() {
    // With a trailing call and result, the plain cut lands on a result for
    // every one of these lengths, so every pass has to move it.
    for turns in 4..8 {
        let mut body = conversation(turns, 1_200);
        body.append(call("x", "grep", r#"{"pattern":"two"}"#));
        body.append(result("x", "g".repeat(1_200)));
        assert_eq!(
            body.messages()[body.len() - PRESERVE_RECENT].role,
            Role::Tool,
            "{turns} turns: the fixture no longer cuts on a result"
        );
        let tail: Vec<Message> = body.messages()[body.len() - PRESERVE_RECENT + 1..].to_vec();
        compact::compact(&mut body, &"p".repeat(16_000), 1).expect("a compaction");
        assert!(no_orphans(&body), "{turns} turns: {body:#?}");
        assert_ne!(body.messages()[2].role, Role::Tool, "{turns} turns");
        assert!(
            body.messages().ends_with(&tail),
            "{turns} turns: the tail changed"
        );
    }
}

/// A summary alone is not summarised again, which would rewrite the prompt for
/// a worse copy of what is already there.
#[test]
fn a_summary_is_not_summarised_again_for_nothing() {
    // The prefix keeps the estimate over the threshold, so it is the summary
    // rule that says no and not the threshold.
    let prefix = "p".repeat(20_000);
    let mut body = conversation(8, 800);
    compact::compact(&mut body, &prefix, 1).expect("a compaction");
    let once = body.clone();
    assert!(compact::compact(&mut body, &prefix, 1).is_none());
    assert_eq!(body, once);
}

/// A second pass after more conversation does run, and the first summary is
/// in the second one's timeline.
#[test]
fn a_later_pass_summarises_the_earlier_summary() {
    let mut body = conversation(8, 800);
    compact::compact(&mut body, "", 1).expect("the first pass");
    for i in 0..4 {
        body.append(Message::user(format!("more {i} {}", "m".repeat(800))));
        body.append(Message::assistant(format!("ok {i} {}", "o".repeat(800))));
    }
    compact::compact(&mut body, "", 1).expect("the second pass");
    assert_eq!(body.len(), 1 + 1 + PRESERVE_RECENT);
    let summary = &body.messages()[1].content;
    assert!(
        summary.contains(&format!("  - user: {PREAMBLE}")),
        "{summary}"
    );
}

/// A pass that would not shrink the prompt does not run.
#[test]
fn a_pass_that_would_not_shrink_the_prompt_does_not_run() {
    let mut body = Body::opening("## The task");
    body.append(Message::user("hi"));
    for i in 0..PRESERVE_RECENT {
        body.append(Message::user(format!("big {i} {}", "x".repeat(4_000))));
    }
    let before = body.clone();
    assert!(compact::compact(&mut body, "", 1).is_none());
    assert_eq!(body, before);
}

/// A result eviction already stubbed shows as that, not as its JSON; and the
/// path a tool was called on is a key file although the arguments are JSON.
#[test]
fn stubs_and_tool_arguments_read_well_in_the_summary() {
    let mut body = Body::opening("## The task");
    body.append(call("s", "read_file", r#"{"path":"crates/x/Cargo.toml"}"#));
    body.append(result("s", stub_body("read_file", 9_000)));
    for m in &conversation(8, 800).messages()[1..] {
        body.append(m.clone());
    }
    compact::compact(&mut body, "", 1).expect("a compaction");
    let summary = &body.messages()[1].content;
    assert!(
        summary.contains("tool_result read_file: [evicted earlier]"),
        "{summary}"
    );
    assert!(!summary.contains("\"evicted\":true"), "{summary}");
    assert!(summary.contains("crates/x/Cargo.toml"), "{summary}");
    assert!(
        summary.contains("- Tools mentioned: read_file."),
        "{summary}"
    );
}

#[test]
fn the_note_says_what_it_did() {
    let mut body = conversation(8, 800);
    let note = compact::compact(&mut body, &"p".repeat(90_000), 40_960)
        .expect("a compaction")
        .note(40_960);
    assert!(
        note.starts_with("compact: 20 earlier message(s) summarised"),
        "{note}"
    );
    assert!(note.ends_with("of a 40960-token window"), "{note}");
}
