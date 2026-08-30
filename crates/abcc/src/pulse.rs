//! 🚨 **One real completion, out of the server that is going to do the work.**
//!
//! F539, and it is the only outage this project has actually had. One hung
//! generation left `/v1/models` answering normally — the id, the state, the
//! whole listing — while **every completion returned nothing for 60 s**, and it
//! took both slots down until `lms load`. `abcc check` polls that listing.
//! **So the check this project already had is precisely the check that missed
//! the only thing it exists to catch**, and no amount of asking the listing
//! harder would have changed that.
//!
//! What settles it is the same request the worker makes, made small: one
//! completion, `max_tokens: 1`, non-streaming, against `/v1/chat/completions`.
//! If the server decodes a token it can generate; if the socket answers and no
//! token is decoded, that is F539's shape and it has its own variant so it
//! cannot be read as either *up* or *down*.
//!
//! # 🚨 F550 — the health question is *did it decode*, not *did it answer*
//!
//! The first version of this file read `choices[0].message.content` and called
//! an empty string silence. Against the champion, **loaded, idle and healthy**,
//! that reported `SILENT after 243 ms`. A positive control in the same shell
//! said why: at `max_tokens` of **1, 8 and 64** the server returns
//! `content: ""` every time and puts **every** token in `reasoning_content`,
//! with `usage.completion_tokens` matching the cap exactly. The champion is a
//! reasoning model; a short completion never reaches the answer channel at all.
//!
//! ▶ So the instrument is **`usage.completion_tokens >= 1`**. Waiting for prose
//! instead is not an option: the trace runs 9,942–16,564 characters on identical
//! input (F246), so the probe that exists to be cheap would become the most
//! expensive thing in the preflight. The channel the tokens came down is still
//! reported, because *the model generated* and *the model answered* are two
//! different sentences (F497).
//!
//! ⚠ **It is a measurement and not a verdict.** The host asked for one token and
//! watched what happened — the same shape as a gate rung, and the opposite shape
//! from the learned rate beside it in the report. That distinction is the point:
//! [`abcc_fleet::breaker`] can compute a pass rate and *cannot* tell a hard task
//! from a dead server. This can.
//!
//! ▶ It still **reports and never gates** (ADR-0010 §4). Nothing here returns a
//! decision and no caller in this crate refuses on it.

use std::time::{Duration, Instant};

use abcc_fleet::breaker::{Pulse, Sample};
use serde_json::{Value, json};

use crate::confirm;

/// How long to wait for one token.
///
/// 🚨 Sized against the failure it is looking for rather than against a good
/// turn: F539's wedge answered nothing for **60 s** and counting, while a
/// healthy one-token completion on the champion measured **243 ms**. A generous
/// margin over the good case is still an order of magnitude inside the bad one.
///
/// ⚠ It is deliberately **not** `Limits::idle_gap` (90 s). That budget covers a
/// 30k-token prefill on a real body — F545's worst wait was 30.3 s, and
/// sequentially — while this body is one sentence, so a wait like that here is
/// already the answer.
pub const PULSE_TIMEOUT: Duration = Duration::from_secs(20);

/// The smallest question with an answer. Short on purpose: a long prompt would
/// measure the prefill, and what is under test is whether the server can decode
/// a token at all.
const ASK: &str = "Reply with one word: ready.";

/// Ask the server for one token and report what happened.
///
/// Never returns an error: every way this can go is a [`Pulse`] variant, because
/// *the server decoded nothing* and *the server was not there* are two different
/// findings and a `Result` would collapse the first into the second.
#[must_use]
pub fn take(base_url: &str, model: &str, api_key: Option<&str>) -> Pulse {
    let url = completions_url(base_url);
    let started = Instant::now();

    let client = match reqwest::blocking::Client::builder()
        .timeout(PULSE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(e) => return unreachable(started, &format!("no http client: {e}")),
    };

    let mut request = client.post(&url).json(&json!({
        "model": model,
        "messages": [{"role": "user", "content": ASK}],
        // 🚨 One. The question is whether a token is decoded, not what it says.
        "max_tokens": 1,
        // Non-streaming on purpose: a stream that opens and delivers nothing is
        // the failure being looked for, and it would arrive here as a successful
        // connection. Non-streaming also reports `usage` natively — F84's
        // `stream_options` caveat is about the streaming dialect only.
        "stream": false,
        "temperature": 0,
    }));
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }

    let response = match request.send() {
        Ok(response) => response,
        Err(e) => return unreachable(started, &e.to_string()),
    };
    let code = response.status().as_u16();
    let body = match response.text() {
        Ok(body) => body,
        Err(e) => return unreachable(started, &e.to_string()),
    };
    if !(200..300).contains(&code) {
        return unreachable(started, &format!("HTTP {code}: {}", clip(&body)));
    }

    read(&body, ms(started))
}

/// Turn one non-streaming completion body into a pulse.
///
/// Split out from the request so that every shape below is a test rather than
/// something a live server has to be persuaded into producing.
#[must_use]
pub fn read(body: &str, elapsed_ms: u64) -> Pulse {
    let Ok(parsed) = serde_json::from_str::<Value>(body) else {
        return Pulse::Silent {
            after_ms: elapsed_ms,
            detail: format!("HTTP 200 with a body that is not JSON: {}", clip(body)),
        };
    };

    // 🚨 The instrument. F550: the answer channel is empty on a reasoning model
    // at any short cap, so what is read is the count the server itself reports.
    let tokens = parsed
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let sample = sample_of(&parsed);

    // ⚠ Text without a count still counts. `completion_tokens` is missing on
    // some proxies, and a token that arrived is a token that arrived — the
    // absence of a counter is not the absence of generation.
    let counted = u32::try_from(tokens).unwrap_or(u32::MAX);
    if counted > 0 || !matches!(sample, Sample::CountOnly) {
        return Pulse::Answered {
            elapsed_ms,
            tokens: counted,
            sample,
        };
    }

    // HTTP 200, zero tokens, no text. This is the wedge, and it is the arm that
    // exists so a 200 cannot be read as health.
    Pulse::Silent {
        after_ms: elapsed_ms,
        detail: format!("HTTP 200 and nothing decoded: {}", clip(body)),
    }
}

/// What text came back, and down which channel.
fn sample_of(parsed: &Value) -> Sample {
    let message = parsed.pointer("/choices/0/message").unwrap_or(&Value::Null);
    let text = |key: &str| {
        message
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    if let Some(answer) = text("content") {
        return Sample::Answer(answer);
    }
    if let Some(reasoning) = text("reasoning_content") {
        return Sample::Reasoning(reasoning);
    }
    Sample::CountOnly
}

fn unreachable(started: Instant, detail: &str) -> Pulse {
    Pulse::Unreachable {
        after_ms: ms(started),
        detail: detail.to_owned(),
    }
}

fn ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The completions endpoint, from whichever of the four spellings of the base
/// url the operator gave. It reuses `confirm`'s root so that the pulse and the
/// listing cannot end up asking two different servers.
#[must_use]
pub fn completions_url(base: &str) -> String {
    let listing = confirm::models_url(base);
    let root = listing.strip_suffix("/v1/models").unwrap_or(&listing);
    format!("{root}/v1/chat/completions")
}

/// Enough of a body to recognise it, and no more.
fn clip(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= 200 {
        return body.to_owned();
    }
    let head: String = body.chars().take(200).collect();
    format!("{head}\u{2026}")
}
