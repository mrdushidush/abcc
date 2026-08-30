//! 🚨 **F550 — the health question is *did the server decode*, not *did it
//! answer*.**
//!
//! Every body below is a real shape. The first is what the champion returns to
//! `max_tokens: 1` — **verbatim, from this box, with the model loaded and idle**
//! — and the first version of `pulse` called it `SILENT after 243 ms`, because
//! it read `message.content` and the champion is a reasoning model that puts
//! every token of a short completion in `reasoning_content`.
//!
//! ⚠ The general lesson is the one this project keeps paying for: **a zero from
//! an unvalidated instrument is not a measurement.** What settled it was a
//! positive control in the same shell — `max_tokens` at 1, 8 and 64, all three
//! with `content: ""` and `completion_tokens` equal to the cap.

use abcc::pulse;
use abcc_fleet::breaker::{Pulse, Sample};

/// 🚨 **Verbatim from `http://127.0.0.1:1234/v1/chat/completions`,
/// `qwen3.6-35b-a3b-mtp@iq3_s`, `max_tokens: 1`, 2026-08-30.** The server is
/// healthy in this body and everything about it says so except the one field a
/// naive check would read.
const CHAMPION_ONE_TOKEN: &str = r#"{
  "id": "chatcmpl-q02bpqi48o7i2ckqx5w1k",
  "object": "chat.completion",
  "created": 1788094395,
  "model": "qwen3.6-35b-a3b-mtp@iq3_s",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "",
        "reasoning_content": "Thinking",
        "tool_calls": []
      },
      "logprobs": null,
      "finish_reason": "length"
    }
  ],
  "usage": {
    "prompt_tokens": 17,
    "completion_tokens": 1,
    "total_tokens": 18,
    "completion_tokens_details": { "reasoning_tokens": 1 }
  },
  "stats": {},
  "system_fingerprint": "qwen3.6-35b-a3b-mtp@iq3_s"
}"#;

/// 🚨🚨 The regression this file exists for. A healthy champion must not read as
/// a wedged server.
#[test]
fn the_champions_one_token_reply_is_a_healthy_pulse() {
    let pulse = pulse::read(CHAMPION_ONE_TOKEN, 243);
    assert!(
        pulse.answered(),
        "a loaded, idle, correctly configured champion read as silent: {pulse}"
    );
    let Pulse::Answered {
        elapsed_ms,
        tokens,
        sample,
    } = pulse
    else {
        unreachable!("checked above")
    };
    assert_eq!(elapsed_ms, 243);
    assert_eq!(tokens, 1);
    // ⚠ And the channel is reported rather than smoothed over: *the model
    // generated* and *the model answered* are two different sentences.
    assert_eq!(sample, Sample::Reasoning("Thinking".to_owned()));
    assert!(sample.to_string().contains("reasoning only"), "{sample}");
}

/// A server that answers in prose is reported as answering in prose.
#[test]
fn an_answer_channel_reply_is_read_as_one() {
    let body = r#"{"choices":[{"message":{"content":"ready"}}],
                   "usage":{"completion_tokens":1}}"#;
    let Pulse::Answered { sample, tokens, .. } = pulse::read(body, 100) else {
        panic!("a token came back and was not seen");
    };
    assert_eq!(tokens, 1);
    assert_eq!(sample, Sample::Answer("ready".to_owned()));
}

/// 🚨 **F539's shape: HTTP 200, and nothing decoded.** This is the one arm that
/// has to stay distinguishable from every other, because it is the only failure
/// the model listing cannot see.
#[test]
fn two_hundred_with_nothing_decoded_is_silence_and_not_health() {
    let body = r#"{"choices":[{"message":{"content":"","reasoning_content":""}}],
                   "usage":{"completion_tokens":0}}"#;
    let pulse = pulse::read(body, 60_000);
    assert!(!pulse.answered(), "{pulse}");
    let Pulse::Silent { after_ms, .. } = pulse else {
        panic!("the wedge was not read as silence");
    };
    assert_eq!(after_ms, 60_000);
}

/// A body that is not a completion at all — a proxy's error page behind a 200 —
/// is silence rather than a parse that quietly succeeds.
#[test]
fn a_two_hundred_that_is_not_a_completion_is_silence() {
    assert!(matches!(
        pulse::read("<html>gateway</html>", 12),
        Pulse::Silent { .. }
    ));
}

/// ⚠ Text without a usage count still counts. `completion_tokens` is missing on
/// some proxies, and the absence of a counter is not the absence of generation.
#[test]
fn text_with_no_usage_count_is_still_a_token() {
    let body = r#"{"choices":[{"message":{"content":"ready"}}]}"#;
    assert!(pulse::read(body, 40).answered());

    // And a count with no text is too, which is the same rule from the other
    // side: the count is the instrument.
    let body = r#"{"choices":[{"message":{"content":""}}],"usage":{"completion_tokens":3}}"#;
    let Pulse::Answered { tokens, sample, .. } = pulse::read(body, 40) else {
        panic!("three decoded tokens read as silence");
    };
    assert_eq!(tokens, 3);
    assert_eq!(sample, Sample::CountOnly);
}

/// The pulse and the listing must reach the same server, whichever of the four
/// spellings of the base url the operator gave.
#[test]
fn every_spelling_of_the_base_url_reaches_the_same_completions_endpoint() {
    for given in [
        "http://127.0.0.1:1234",
        "http://127.0.0.1:1234/",
        "http://127.0.0.1:1234/v1",
        "http://127.0.0.1:1234/v1/models",
        "http://127.0.0.1:1234/v1/chat/completions",
    ] {
        assert_eq!(
            pulse::completions_url(given),
            "http://127.0.0.1:1234/v1/chat/completions",
            "{given}"
        );
    }
}

// ---------------------------------------------------------------------------
// 🚨 2026-08-30 — David's ruling: a silent pulse REFUSES a preflight
// ---------------------------------------------------------------------------

/// 🚨 **`refuses` is not `!answered`, and the gap between them is `NotTaken`.**
///
/// This is the assertion the ruling turns on. ADR-0010 §4 keeps the *rate* from
/// gating because a statistical verdict cannot tell a hard task from a dead
/// server; a pulse can, so it may refuse. But *nobody asked* is a third thing,
/// and letting it refuse would read an absence as a failure — which is the one
/// confusion `Outcome` exists to prevent, applied one level out.
#[test]
fn an_untaken_pulse_never_refuses_and_that_is_the_point_of_the_variant() {
    assert!(!Pulse::NotTaken.answered());
    assert!(
        !Pulse::NotTaken.refuses(),
        "an absence refused a preflight — nothing was measured, so nothing failed"
    );

    let silent = Pulse::Silent {
        after_ms: 60_000,
        detail: "HTTP 200 and nothing decoded".to_owned(),
    };
    let unreachable = Pulse::Unreachable {
        after_ms: 20_000,
        detail: "connection closed".to_owned(),
    };
    for measured in [&silent, &unreachable] {
        assert!(measured.refuses(), "{measured}");
        assert!(!measured.answered(), "{measured}");
    }
}

/// The champion's own healthy body must not refuse anything.
///
/// ⚠ The same body as the F550 regression above, asserted through the new
/// method: the ruling made a wrong instrument able to stop every run on this
/// machine, so the instrument is re-checked at the place that now acts on it.
#[test]
fn the_champions_healthy_pulse_does_not_refuse_a_preflight() {
    let pulse = pulse::read(CHAMPION_ONE_TOKEN, 243);
    assert!(
        !pulse.refuses(),
        "a loaded, idle, fingerprint-confirmed champion would have stopped the run: {pulse}"
    );
}

/// **The refusal has to be actionable**: what happened, the pulse itself, and
/// the command that fixes it.
#[test]
fn a_refusal_says_what_happened_and_what_to_do_about_it() {
    let silent = Pulse::Silent {
        after_ms: 60_000,
        detail: "HTTP 200 and nothing decoded: {}".to_owned(),
    };
    let advice = pulse::refusal_advice(&silent);

    assert!(advice.contains("60000 ms"), "{advice}");
    assert!(advice.contains("F539"), "{advice}");
    assert!(advice.contains("lms load"), "{advice}");
    // ⚠ The line that keeps the two halves of `abcc breaker` apart. A reader who
    // learns the pulse can refuse will assume the rate can too.
    assert!(
        advice.contains("breaker") && advice.contains("never gates"),
        "the refusal does not say the rate still cannot gate: {advice}"
    );
}

/// An answered pulse produces no advice worth printing, and asking for it is not
/// a panic.
///
/// ⚠ Answered rather than asserted, deliberately: `refusal_advice` cannot see
/// its caller, and a `unreachable!()` here would be this function having an
/// opinion about one.
#[test]
fn advice_for_a_pulse_that_did_not_refuse_is_a_sentence_and_not_a_panic() {
    let advice = pulse::refusal_advice(&Pulse::NotTaken);
    assert!(advice.contains("did not refuse"), "{advice}");
}
