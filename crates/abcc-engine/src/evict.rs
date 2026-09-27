//! Stale tool results, stubbed before the server has to cut — PLAN-TOOL B3.
//!
//! Ported from claudette `crates/claudette/src/runtime/context_evict.rs`
//! (David's code; PLAN-TOOL §5 decision 2 (b), ruled 2026-09-27). Its thresholds
//! and its stub text are kept. Three things differ, each for an abcc reason:
//!
//! * **No current-turn boundary.** claudette never touches anything after the
//!   last user message. An abcc phase is one user message (the brief) followed
//!   by the whole phase, so that rule would never fire here. Recency immunity
//!   (the last [`KEEP_RECENT`] results) is the protection, and the brief is
//!   never touched because it is not a tool result.
//! * **The [`Body`] itself is stubbed, not a wire copy.** The re-read guard
//!   (`TurnLoop::already_have`) looks for an earlier copy in the body and
//!   withholds the bytes when it finds one. A wire-only stub would leave it
//!   pointing the model at a copy the model can no longer see. Stubbed in the
//!   body, the earlier copy stops matching and a re-read is served whole.
//!   Nothing durable is lost: every result is on the log as `ToolCallEnded`.
//! * **It evicts down to a low-water mark** ([`LOW_WATER_PERCENT`]), not to just
//!   under the trigger. Stubbing a message changes the prompt from that point
//!   on, so the server re-reads everything after it (W1: the prefix cache is
//!   79.7% of TTFT). Stopping at the trigger would pay that on nearly every turn
//!   once a phase sits near the line.
//!
//! Why at all: past the window LM Studio's `truncateMiddle` cuts the middle of
//! the conversation at HTTP 200 and orphans tool results (F748, F775). This is
//! abcc choosing what goes first instead.

use std::collections::HashMap;

use crate::provider::{Body, Message, Role};

/// The most recent tool results, which are never stubbed.
pub const KEEP_RECENT: usize = 8;

/// A result shorter than this is not worth a stub.
pub const MIN_EVICTABLE_CHARS: usize = 512;

/// Eviction starts when the estimate reaches this share of the window.
pub const TRIGGER_PERCENT: usize = 60;

/// And stubs oldest-first until the estimate is under this share.
pub const LOW_WATER_PERCENT: usize = 45;

/// How a stub begins, so a stubbed result is never stubbed again.
pub const STUB_MARKER: &str = "{\"evicted\":true";

/// claudette's stub, word for word: what was here, and do not re-run it.
#[must_use]
pub fn stub_body(tool: &str, original_chars: usize) -> String {
    let tool = serde_json::to_string(tool).unwrap_or_else(|_| "\"tool\"".to_owned());
    format!(
        "{{\"evicted\":true,\"tool\":{tool},\"original_chars\":{original_chars},\"note\":\"Stale \
         output from an earlier turn, cleared to free context. Anything decided from it is \
         already reflected in the conversation. Do NOT re-run the tool just to restore this \
         text — only re-run it if a NEW step genuinely needs the raw content.\"}}"
    )
}

/// Tokens, estimated at four characters each: the head's prefix plus the body.
#[must_use]
pub fn estimate_tokens(prefix: &str, body: &Body) -> usize {
    prefix.len() / 4 + 1 + body.messages().iter().map(message_tokens).sum::<usize>()
}

pub(crate) fn message_tokens(m: &Message) -> usize {
    let calls: usize = m
        .tool_calls
        .iter()
        .map(|c| (c.tool.len() + c.arguments.len()) / 4 + 1)
        .sum();
    m.content.len() / 4 + 1 + calls
}

/// What one pass stubbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evicted {
    pub results: usize,
    /// Characters of tool output replaced.
    pub chars: usize,
    /// The estimate before and after, in tokens.
    pub before: usize,
    pub after: usize,
}

impl Evicted {
    /// The line the log carries.
    #[must_use]
    pub fn note(&self, window: usize) -> String {
        format!(
            "evict: {} stale tool result(s) stubbed, {} chars; estimate {} -> {} tokens of a \
             {window}-token window",
            self.results, self.chars, self.before, self.after
        )
    }
}

/// Stub stale tool results in `body` if the estimate has reached
/// [`TRIGGER_PERCENT`] of `window`. `None` when nothing was stubbed.
pub fn stale(body: &mut Body, prefix: &str, window: usize) -> Option<Evicted> {
    let trigger = window.saturating_mul(TRIGGER_PERCENT) / 100;
    let low = window.saturating_mul(LOW_WATER_PERCENT) / 100;
    let before = estimate_tokens(prefix, body);
    if before < trigger {
        return None;
    }

    let names: HashMap<String, String> = body
        .messages()
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .map(|c| (c.id.clone(), c.tool.clone()))
        .collect();
    let messages = body.messages_mut();
    let results: Vec<usize> = (0..messages.len())
        .filter(|&i| messages[i].role == Role::Tool)
        .collect();
    let stale = results.len().saturating_sub(KEEP_RECENT);

    let mut estimate = before;
    let mut evicted = Evicted {
        results: 0,
        chars: 0,
        before,
        after: before,
    };
    for &i in &results[..stale] {
        if estimate < low {
            break;
        }
        let m = &mut messages[i];
        if m.content.len() < MIN_EVICTABLE_CHARS || m.content.starts_with(STUB_MARKER) {
            continue;
        }
        let tool = m
            .tool_call_id
            .as_deref()
            .and_then(|id| names.get(id))
            .map_or("tool", String::as_str);
        let stub = stub_body(tool, m.content.len());
        estimate = estimate.saturating_sub(m.content.len().saturating_sub(stub.len()) / 4);
        evicted.results += 1;
        evicted.chars += m.content.len();
        m.content = stub;
    }
    evicted.after = estimate;
    (evicted.results > 0).then_some(evicted)
}
