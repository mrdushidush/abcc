//! A long conversation, summarised before the window fills — PLAN-TOOL C1's
//! compaction, the chat half of B3.
//!
//! Ported from claudette `crates/claudette/src/runtime/compact.rs` and the
//! threshold in `run/compaction_policy.rs` (David's code; PLAN-TOOL §5 decision
//! 2 (b), ruled 2026-09-27). The summary is claudette's heuristic one, with no
//! model call, and its continuation text is kept word for word. What differs,
//! each for an abcc reason:
//!
//! * **The brief stays.** claudette's system prompt lives outside the session.
//!   Here the frozen prefix is the [`crate::Head`] and the task itself is the
//!   body's first message, so the summary goes *after* it. Summarising it would
//!   cost the model the one message that says what the work is.
//! * **The summary is a user message.** A body has no system role (ADR-0011 §2:
//!   the only system text is the head).
//! * **A tool result cut off from its call is summarised, not dropped.**
//!   claudette deletes a kept result whose call went into the summary. Here the
//!   cut moves past it instead, so it is in the timeline and the dialect never
//!   sees a result arriving from nowhere.
//! * **A pass that would not shrink the prompt does not run.** Every pass
//!   rewrites the prompt after the brief, which the server re-reads in full
//!   (W1: the prefix cache is 79.7% of TTFT), so it has to buy something.
//! * **No images.** abcc sends none, so the image eviction is not ported.
//! * **One estimate.** [`evict::estimate_tokens`], the head's prefix plus the
//!   body, so eviction and compaction measure the same thing.
//! * **Paths from tool arguments.** They are JSON, so claudette's whitespace
//!   split never finds them; the `path` field is read instead. `.py` and
//!   `.toml` join the extensions, for the gate's two toolchains.
//!
//! Where it runs: between the operator's turns, in `Driver::chat`, as claudette
//! compacts between REPL turns. Inside a turn, [`crate::evict`] keeps the window.
//!
//! ⚠ A second pass summarises the first summary as one timeline line, as
//! claudette's does.

use std::collections::HashMap;

use crate::evict::{self, STUB_MARKER};
use crate::provider::{Body, Message, Role};

/// Messages at the end kept word for word: claudette's `HARD_COMPACT_PRESERVE`.
pub const PRESERVE_RECENT: usize = 12;

/// The threshold never goes under this, whatever the window.
pub const FLOOR_TOKENS: usize = 4_000;

/// Nor over this.
pub const CEILING_TOKENS: usize = 1_000_000;

/// How a summary begins, so a body can tell one from what the operator said.
pub const PREAMBLE: &str = "This session is being continued from a previous conversation that \
                            ran out of context.";

/// Half the window, as claudette's `resolve_compact_threshold`. The other half
/// is headroom for the reply and for a turn's own tool results.
#[must_use]
pub fn threshold(window: usize) -> usize {
    (window / 2).clamp(FLOOR_TOKENS, CEILING_TOKENS)
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compacted {
    /// Messages replaced by the summary.
    pub removed: usize,
    /// The estimate before and after, in tokens.
    pub before: usize,
    pub after: usize,
}

impl Compacted {
    /// The line the log carries.
    #[must_use]
    pub fn note(&self, window: usize) -> String {
        format!(
            "compact: {} earlier message(s) summarised; estimate {} -> {} tokens of a \
             {window}-token window",
            self.removed, self.before, self.after
        )
    }
}

/// Summarise everything between the brief and the last [`PRESERVE_RECENT`]
/// messages if the estimate has reached [`threshold`]. `None` when nothing
/// was done.
pub fn compact(body: &mut Body, prefix: &str, window: usize) -> Option<Compacted> {
    let before = evict::estimate_tokens(prefix, body);
    if before < threshold(window) {
        return None;
    }

    let messages = body.messages();
    let mut keep_from = messages.len().saturating_sub(PRESERVE_RECENT).max(1);
    while keep_from < messages.len() && messages[keep_from].role == Role::Tool {
        keep_from += 1;
    }
    let removed = &messages[1..keep_from];
    if removed.is_empty() || (removed.len() == 1 && is_summary(&removed[0])) {
        return None;
    }

    let names: HashMap<&str, &str> = messages
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .map(|c| (c.id.as_str(), c.tool.as_str()))
        .collect();
    let summary = Message::user(continuation(
        &summarize_messages(removed, &names),
        true,
        keep_from < messages.len(),
    ));
    let freed: usize = removed.iter().map(evict::message_tokens).sum();
    if evict::message_tokens(&summary) >= freed {
        return None;
    }

    let removed = removed.len();
    body.summarise(1..keep_from, summary);
    Some(Compacted {
        removed,
        before,
        after: evict::estimate_tokens(prefix, body),
    })
}

/// Whether `message` is a summary an earlier pass wrote.
#[must_use]
pub fn is_summary(message: &Message) -> bool {
    message.role == Role::User && message.content.starts_with(PREAMBLE)
}

/// claudette's `format_compact_summary`: drop any `<analysis>`, and turn
/// `<summary>` into a heading.
#[must_use]
pub fn format_summary(summary: &str) -> String {
    let without_analysis = strip_tag_block(summary, "analysis");
    let formatted = if let Some(content) = extract_tag_block(&without_analysis, "summary") {
        without_analysis.replace(
            &format!("<summary>{content}</summary>"),
            &format!("Summary:\n{}", content.trim()),
        )
    } else {
        without_analysis
    };

    collapse_blank_lines(&formatted).trim().to_string()
}

/// claudette's `get_compact_continuation_message`, word for word.
#[must_use]
pub fn continuation(
    summary: &str,
    suppress_follow_up_questions: bool,
    recent_messages_preserved: bool,
) -> String {
    let mut base = format!(
        "{PREAMBLE} The summary below covers the earlier portion of the conversation.\n\n{}",
        format_summary(summary)
    );

    if recent_messages_preserved {
        base.push_str("\n\nRecent messages are preserved verbatim.");
    }

    base.push_str(
        "\n\nThe summary above is lossy and may omit the RESULT of an action that was still in \
         progress. Do NOT assume any step (a file edit, a commit, a push, opening a PR, a test \
         run) finished just because it appears above — re-check the current state with a tool \
         before reporting that step as done.",
    );

    if suppress_follow_up_questions {
        base.push_str(
            "\nContinue the conversation from where it left off without asking the user any \
             further questions. Resume directly — do not acknowledge the summary, do not recap \
             what was happening, and do not preface with continuation text.",
        );
    }

    base
}

fn summarize_messages(messages: &[Message], names: &HashMap<&str, &str>) -> String {
    let count = |role: Role| messages.iter().filter(|m| m.role == role).count();

    let mut tool_names = messages
        .iter()
        .flat_map(|m| {
            let asked = m.tool_calls.iter().map(|c| c.tool.as_str());
            let answered = tool_name(m, names);
            asked.chain(answered)
        })
        .collect::<Vec<_>>();
    tool_names.sort_unstable();
    tool_names.dedup();

    let mut lines = vec![
        "<summary>".to_string(),
        "Conversation summary:".to_string(),
        format!(
            "- Scope: {} earlier messages compacted (user={}, assistant={}, tool={}).",
            messages.len(),
            count(Role::User),
            count(Role::Assistant),
            count(Role::Tool)
        ),
    ];

    if !tool_names.is_empty() {
        lines.push(format!("- Tools mentioned: {}.", tool_names.join(", ")));
    }

    let recent_user_requests = collect_recent_role_summaries(messages, Role::User, 3);
    if !recent_user_requests.is_empty() {
        lines.push("- Recent user requests:".to_string());
        lines.extend(
            recent_user_requests
                .into_iter()
                .map(|request| format!("  - {request}")),
        );
    }

    let pending_work = infer_pending_work(messages);
    if !pending_work.is_empty() {
        lines.push("- Pending work:".to_string());
        lines.extend(pending_work.into_iter().map(|item| format!("  - {item}")));
    }

    let key_files = collect_key_files(messages);
    if !key_files.is_empty() {
        lines.push(format!("- Key files referenced: {}.", key_files.join(", ")));
    }

    if let Some(current_work) = infer_current_work(messages) {
        lines.push(format!("- Current work: {current_work}"));
    }

    lines.push("- Key timeline:".to_string());
    for message in messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        lines.push(format!("  - {role}: {}", summarize_message(message, names)));
    }
    lines.push("</summary>".to_string());
    lines.join("\n")
}

/// The tool a result answers, named through its call.
fn tool_name<'a>(message: &Message, names: &HashMap<&str, &'a str>) -> Option<&'a str> {
    if message.role != Role::Tool {
        return None;
    }
    let id = message.tool_call_id.as_deref()?;
    names.get(id).copied()
}

/// One timeline entry: claudette's `summarize_block` over each part of a
/// message, joined as its blocks are.
fn summarize_message(message: &Message, names: &HashMap<&str, &str>) -> String {
    let mut parts = Vec::new();
    if message.role == Role::Tool {
        let tool = tool_name(message, names).unwrap_or("tool");
        // A stub is JSON boilerplate; 160 characters of it tell nobody anything.
        let output = if message.content.starts_with(STUB_MARKER) {
            "[evicted earlier]"
        } else {
            message.content.as_str()
        };
        parts.push(truncate_summary(
            &format!("tool_result {tool}: {output}"),
            160,
        ));
    } else {
        if !message.content.is_empty() {
            parts.push(truncate_summary(&message.content, 160));
        }
        for call in &message.tool_calls {
            parts.push(truncate_summary(
                &format!("tool_use {}({})", call.tool, call.arguments),
                160,
            ));
        }
    }
    parts.join(" | ")
}

fn collect_recent_role_summaries(messages: &[Message], role: Role, limit: usize) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == role)
        .rev()
        .filter_map(first_text)
        .take(limit)
        .map(|text| truncate_summary(text, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn infer_pending_work(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text)
        .filter(|text| {
            let lowered = text.to_ascii_lowercase();
            lowered.contains("todo")
                || lowered.contains("next")
                || lowered.contains("pending")
                || lowered.contains("follow up")
                || lowered.contains("remaining")
        })
        .take(3)
        .map(|text| truncate_summary(text, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn collect_key_files(messages: &[Message]) -> Vec<String> {
    let mut files = messages
        .iter()
        .flat_map(|m| extract_file_candidates(&m.content))
        .collect::<Vec<_>>();
    for call in messages.iter().flat_map(|m| m.tool_calls.iter()) {
        files.extend(extract_file_candidates(&call.arguments));
        let path = serde_json::from_str::<serde_json::Value>(&call.arguments)
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_owned));
        files.extend(path);
    }
    files.sort();
    files.dedup();
    files.into_iter().take(8).collect()
}

fn infer_current_work(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text)
        .find(|text| !text.trim().is_empty())
        .map(|text| truncate_summary(text, 200))
}

/// The words of a message, when it has any. A tool result has none: in
/// claudette it is a `ToolResult` block and never a `Text` one.
fn first_text(message: &Message) -> Option<&str> {
    (message.role != Role::Tool && !message.content.trim().is_empty())
        .then_some(message.content.as_str())
}

fn has_interesting_extension(candidate: &str) -> bool {
    std::path::Path::new(candidate)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["rs", "ts", "tsx", "js", "json", "md", "py", "toml"]
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

fn extract_file_candidates(content: &str) -> Vec<String> {
    content
        .split_whitespace()
        .filter_map(|token| {
            let candidate = token.trim_matches(|char: char| {
                matches!(char, ',' | '.' | ':' | ';' | ')' | '(' | '"' | '\'' | '`')
            });
            if candidate.contains('/') && has_interesting_extension(candidate) {
                Some(candidate.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn truncate_summary(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let mut truncated = content.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

fn extract_tag_block(content: &str, tag: &str) -> Option<String> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let start_index = content.find(&start)? + start.len();
    let end_index = content[start_index..].find(&end)? + start_index;
    Some(content[start_index..end_index].to_string())
}

fn strip_tag_block(content: &str, tag: &str) -> String {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    if let (Some(start_index), Some(end_index_rel)) = (content.find(&start), content.find(&end)) {
        let end_index = end_index_rel + end.len();
        let mut stripped = String::new();
        stripped.push_str(&content[..start_index]);
        stripped.push_str(&content[end_index..]);
        stripped
    } else {
        content.to_string()
    }
}

fn collapse_blank_lines(content: &str) -> String {
    let mut result = String::new();
    let mut last_blank = false;
    for line in content.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && last_blank {
            continue;
        }
        result.push_str(line);
        result.push('\n');
        last_blank = is_blank;
    }
    result
}
