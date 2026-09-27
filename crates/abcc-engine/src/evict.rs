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
//! And one thing claudette's eviction does not have at all, **the emergency
//! tier**. When the stale pass is done and the estimate is still at the
//! trigger, the recent results go too, oldest first, down to the same low water,
//! and never the newest one. Recency immunity assumes one result is small next
//! to the window, and here it is not: `read_file` hands back up to 64 KiB
//! (`MAX_READ_BYTES`, about 16k tokens) and the trigger at a 40,960 window is
//! 24,576 tokens, exactly the window less the 16,384 completion budget. Two or
//! three big reads in one turn cross it with nothing yet stale, and then neither
//! the stale pass nor compaction (which runs between operator turns) can act
//! before the server cuts. claudette has 30 of its 112 files over 32 KB and six
//! over 64 KiB. They are counted apart ([`Evicted::recent`]) so the log shows
//! each time the window overrode recency, and their stub
//! ([`recent_stub_body`]) does not call them stale, because the model may not
//! have read them yet.
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

/// abcc's stub for a recent result the emergency tier cleared. Not claudette's
/// words: the model may not have read this one yet, so it is not called stale,
/// and the way back is a smaller part rather than the same call again.
#[must_use]
pub fn recent_stub_body(tool: &str, original_chars: usize) -> String {
    let tool = serde_json::to_string(tool).unwrap_or_else(|_| "\"tool\"".to_owned());
    format!(
        "{{\"evicted\":true,\"tool\":{tool},\"original_chars\":{original_chars},\"note\":\"Recent \
         output, cleared because the conversation had outgrown the context window. If a step \
         still needs it, re-run the tool on a smaller part (for read_file, a from_line..to_line \
         range), not the whole thing again.\"}}"
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
    /// Stale results: older than the last [`KEEP_RECENT`].
    pub results: usize,
    /// Recent results the emergency tier stubbed because the stale ones were
    /// not enough. Zero unless the window overrode recency.
    pub recent: usize,
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
        let recent = if self.recent > 0 {
            format!(
                " and {} recent one(s) stubbed, the stale ones not being enough",
                self.recent
            )
        } else {
            " stubbed".to_owned()
        };
        format!(
            "evict: {} stale tool result(s){recent}, {} chars; estimate {} -> {} tokens of a \
             {window}-token window",
            self.results, self.chars, self.before, self.after
        )
    }
}

/// Stub stale tool results in `body` if the estimate has reached
/// [`TRIGGER_PERCENT`] of `window`, oldest first, down to [`LOW_WATER_PERCENT`].
/// If that is not enough to get under the trigger, the recent ones go as well
/// (the emergency tier), never the newest. `None` when nothing was stubbed.
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

    let mut evicted = Evicted {
        results: 0,
        recent: 0,
        chars: 0,
        before,
        after: before,
    };
    for &i in &results[..stale] {
        if evicted.after < low {
            break;
        }
        if let Some(chars) = stub(&mut messages[i], &names, stub_body, &mut evicted.after) {
            evicted.results += 1;
            evicted.chars += chars;
        }
    }
    // The emergency tier: the window overrides recency, but never the newest.
    if evicted.after >= trigger {
        let newest = results.len().saturating_sub(1);
        for &i in &results[stale.min(newest)..newest] {
            if evicted.after < low {
                break;
            }
            if let Some(chars) = stub(
                &mut messages[i],
                &names,
                recent_stub_body,
                &mut evicted.after,
            ) {
                evicted.recent += 1;
                evicted.chars += chars;
            }
        }
    }
    (evicted.results + evicted.recent > 0).then_some(evicted)
}

/// Replace one result with `stub_with`'s stub, unless it is too small to be
/// worth one or is a stub already, and take what that frees off `estimate`.
/// The characters replaced, when it was stubbed.
fn stub(
    m: &mut Message,
    names: &HashMap<String, String>,
    stub_with: fn(&str, usize) -> String,
    estimate: &mut usize,
) -> Option<usize> {
    let chars = m.content.len();
    if chars < MIN_EVICTABLE_CHARS || m.content.starts_with(STUB_MARKER) {
        return None;
    }
    let tool = m
        .tool_call_id
        .as_deref()
        .and_then(|id| names.get(id))
        .map_or("tool", String::as_str);
    let stub = stub_with(tool, chars);
    *estimate = estimate.saturating_sub(chars.saturating_sub(stub.len()) / 4);
    m.content = stub;
    Some(chars)
}
