//! One event is one line, and a `Why` does not get to be the exception.
//!
//! 🚨 **F501, found by looking at the screen.** The reader had been folded over
//! the real 151-event log programmatically and every assertion passed; the
//! defect was only visible once a person ran `abcc watch` in a terminal. Seq 99
//! of that log carries a provider's HTML error page inside
//! [`Why::EngineError`], and `describe` interpolated it whole: ten unaligned
//! rows out of the `seq · elapsed · text` columns and off the right edge of the
//! frame, hiding the events after it.
//!
//! The claims here are about **line discipline**, which is the one property a
//! fold-based test cannot see and a screenshot can.

use abcc_core::attempt::AttemptOutcome;
use abcc_core::event::{Event, Logged};
use abcc_core::outcome::{Outcome, Why};
use abcc_core::redact::Secrets;
use abcc_core::seq::{AttemptId, Seq, TaskId};
use abcc_tui::line::CLIP;
use abcc_tui::{Theme, describe};

/// Seq 99 of the real log, verbatim. LM Studio answers a request that overflows
/// its window with this, and nothing in it says so.
const REAL_500: &str = "local answered HTTP 500: <!DOCTYPE html>\n<html lang=\"en\">\n<head>\n\
                        <meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n\
                        <pre>Internal Server Error</pre>\n</body>\n</html>\n";

fn line_for(event: Event) -> String {
    describe(
        &Logged {
            seq: Seq::new(99),
            at_ms: 0,
            event,
        },
        Theme::Command,
    )
    .text
}

/// The line must survive the worst free text the log actually holds.
fn assert_one_line(text: &str) {
    assert!(
        !text.chars().any(char::is_control),
        "a control character reached the feed and moved the cursor: {text:?}"
    );
    // The prefix each arm writes is this program's own words and is short; what
    // is bounded here is the part that came from somewhere else.
    assert!(
        text.chars().count() <= CLIP * 2,
        "one event took {} characters of a feed that shows one line each: {text:?}",
        text.chars().count()
    );
}

#[test]
fn an_engine_errors_html_page_does_not_escape_its_line() {
    let text = line_for(Event::AttemptEnded {
        task: TaskId::at(Seq::new(2)),
        attempt: AttemptId::at(Seq::new(8)),
        outcome: AttemptOutcome::HardFailure {
            why: Why::EngineError {
                detail: REAL_500.to_owned(),
            },
        },
    });
    assert_one_line(&text);
    assert!(
        text.contains("hard failure"),
        "the class was lost along with the newlines: {text}"
    );
}

/// The same defect on the other two paths a `Why` reaches the screen. Seq 95 of
/// the real log is a `ToolCallEnded` whose detail ran off the frame the same
/// way, so this is not a hypothetical second case.
#[test]
fn a_tools_unmeasured_ending_and_a_rung_are_clipped_too() {
    let tool = line_for(Event::ToolCallEnded {
        attempt: AttemptId::at(Seq::new(8)),
        tool: "write_file".to_owned(),
        exit: None,
        elapsed_ms: 0,
        unmeasured: Some(Why::EngineError {
            detail: REAL_500.to_owned(),
        }),
        // F505 keeps the refused arguments on the log; F501 keeps them off the
        // feed. A whole diff here must not widen the line.
        arguments: Some(Secrets::default().scrub(REAL_500).text),
        // ▶ And F713 puts the tool's *output* on the log beside them, which
        // is the bigger of the two — `MAX_READ_BYTES` is 64 KiB. Same rule, and
        // it is asserted here rather than assumed: one event is one line.
        output: Some(Secrets::default().scrub(REAL_500).text),
    });
    assert_one_line(&tool);

    let rung = line_for(Event::RungRecorded {
        attempt: AttemptId::at(Seq::new(8)),
        outcome: Outcome::Unmeasured {
            rung: "tests".to_owned(),
            why: Why::EngineError {
                detail: REAL_500.to_owned(),
            },
        },
    });
    assert_one_line(&rung);
}

/// The two endings the first real runs produced, as an operator reads them.
///
/// ⚠ Asserted on the *sentence*, not on `Debug`: the module's second rule is
/// that no `Debug` reaches the screen, and a variant name leaking into the feed
/// is how that rule stops holding.
#[test]
fn the_two_new_endings_read_as_sentences() {
    let nothing = line_for(Event::AttemptEnded {
        task: TaskId::at(Seq::new(101)),
        attempt: AttemptId::at(Seq::new(107)),
        outcome: AttemptOutcome::Uncertain {
            why: Why::SaidNothing {
                by: "Recon".to_owned(),
                completion_tokens: 47,
                reasoning_tokens: Some(42),
            },
        },
    });
    assert_one_line(&nothing);
    assert!(
        nothing.contains("said nothing") && nothing.contains("Recon"),
        "the operator cannot tell who said nothing: {nothing}"
    );

    let overflow = line_for(Event::AttemptEnded {
        task: TaskId::at(Seq::new(2)),
        attempt: AttemptId::at(Seq::new(8)),
        outcome: AttemptOutcome::Uncertain {
            why: Why::ContextOverflow {
                window: 16_384,
                prompt_tokens: 14_261,
            },
        },
    });
    assert_one_line(&overflow);
    assert!(
        overflow.contains("16384"),
        "the window is the one number that makes this actionable: {overflow}"
    );
}

/// 🚨 **F708 put the largest free text this log holds on it, and it is a
/// document.**
///
/// A phase's opening brief carries the task, the operator's redirect, the
/// paragraph naming what refused the tree it inherited, and — for the Judge — a
/// whole diff, up to `abcc_gate::judge::MAX_PATCH_CHARS` of it. Interpolating
/// that into the feed is F501 again with a thousand lines in place of ten. So the
/// arm renders the size and nothing else: the `attempt_phase_entered` directly
/// above it already says what was being asked, and the text is on the log for a
/// reader who wants it.
#[test]
fn a_recorded_brief_is_a_document_and_it_still_takes_one_line() {
    let brief = REAL_500.repeat(400);
    let text = line_for(Event::BriefRecorded {
        attempt: AttemptId::at(Seq::new(8)),
        text: Secrets::default().scrub(brief.clone()).text,
    });
    assert_one_line(&text);
    assert!(
        text.contains("brief") && text.contains(&brief.len().to_string()),
        "the feed does not say a brief was recorded or how big it was: {text}"
    );
}

/// 🚨 **F718: a field written and no reader is the same defect F708 was.**
///
/// F715 put the sampler's seed on `ModelCallStarted` because five identical
/// requests to the champion had been five distinct answers and nothing on the
/// log could say which draw produced which. The feed then rendered the model,
/// the head and the budget — all three of which are *constant across a phase* —
/// so two consecutive calls printed the same line, and the one value that
/// distinguishes them, the one a re-flight needs, was on the record and on no
/// screen.
///
/// ⚠ The assertion is on the seed's own digits rather than on the word `seed`:
/// a line that said `seed` and interpolated something else would pass the
/// weaker test, and *which* seed is the entire content of the field.
#[test]
fn a_model_call_line_names_its_seed() {
    let text = line_for(Event::ModelCallStarted {
        attempt: AttemptId::at(Seq::new(8)),
        provider: "openai-compat".to_owned(),
        model: "qwen3.6-35b-a3b-mtp@iq3_s".to_owned(),
        head: "builders".to_owned(),
        head_digest: "9d491d116cf78300".to_owned(),
        ceiling: "exec".to_owned(),
        budget: 16_384,
        seed: 2_859_510_835,
    });
    assert_one_line(&text);
    assert!(
        text.contains("2859510835"),
        "the seed is on the log and not on the screen: {text}"
    );

    // The negative control, and it is the reason this test is two calls rather
    // than one: everything else on the line is frozen per phase (ADR-0011 freezes
    // the head; the budget is a compile-time constant), so a renderer that
    // dropped the seed would still produce two *identical* lines here and a
    // test asserting only "contains seed" would not notice.
    let next = line_for(Event::ModelCallStarted {
        attempt: AttemptId::at(Seq::new(8)),
        provider: "openai-compat".to_owned(),
        model: "qwen3.6-35b-a3b-mtp@iq3_s".to_owned(),
        head: "builders".to_owned(),
        head_digest: "9d491d116cf78300".to_owned(),
        ceiling: "exec".to_owned(),
        budget: 16_384,
        seed: 1_612_451_988,
    });
    assert_ne!(
        text, next,
        "two calls of one phase render identically, so the feed cannot tell \
         a retry from its parent"
    );
}

/// 🚨 **F718's other half, and `None` is the case that earns the test.**
///
/// F713 put a tool's output on `ToolCallEnded`; F717 then measured what it
/// weighs — 244 tokens at the median, 984 at the mean — so the size is the one
/// thing a one-line feed can honestly say about it. ⚠ But `output` is
/// `#[serde(default)]`, so all **2,147** tool calls this project logged before
/// F713 replay as `None`, and rendering those as `0 chars` would put *the tool
/// returned nothing* on two thousand rows whose truth is that nobody wrote it
/// down. That is F494's rule — absent, never zero — and here it is load-bearing
/// rather than pedantic, because the archive is three orders of magnitude
/// larger than the recorded part.
#[test]
fn a_tool_line_sizes_its_output_and_says_when_nobody_recorded_one() {
    let body = "src/\nsrc/main.rs\nsrc/cli.rs";
    let sized = line_for(Event::ToolCallEnded {
        attempt: AttemptId::at(Seq::new(8)),
        tool: "list_files".to_owned(),
        exit: Some(0),
        elapsed_ms: 40,
        unmeasured: None,
        arguments: None,
        output: Some(Secrets::default().scrub(body).text),
    });
    assert_one_line(&sized);
    assert!(
        sized.contains(&body.len().to_string()),
        "the feed does not say what the tool handed the model: {sized}"
    );

    let archived = line_for(Event::ToolCallEnded {
        attempt: AttemptId::at(Seq::new(8)),
        tool: "list_files".to_owned(),
        exit: Some(0),
        elapsed_ms: 40,
        unmeasured: None,
        arguments: None,
        output: None,
    });
    assert_one_line(&archived);
    assert!(
        !archived.contains('0') || !archived.contains("0 chars"),
        "a call nobody recorded reads as a tool that returned nothing: {archived}"
    );
    assert!(
        archived.contains("not recorded"),
        "the feed cannot tell an unrecorded output from an empty one: {archived}"
    );

    // And an output that really was empty is the third state, distinct from both.
    let empty = line_for(Event::ToolCallEnded {
        attempt: AttemptId::at(Seq::new(8)),
        tool: "bash".to_owned(),
        exit: Some(0),
        elapsed_ms: 12,
        unmeasured: None,
        arguments: None,
        output: Some(Secrets::default().scrub("").text),
    });
    assert_one_line(&empty);
    assert_ne!(
        empty.replace("bash", "list_files"),
        archived,
        "a tool that said nothing and a call nobody recorded render the same"
    );
}

/// 🚨 **The size is deliberately absent when the call had no measurable ending,
/// and this test is the record of that trade.**
///
/// F501's bar is `CLIP * 2` = 112 characters, and the unmeasured arm was already
/// spending 103 of them: the tool, the sentence `no measurable ending:`, a
/// `CLIP`-clipped [`Why`] and the elapsed time. Appending a size pushed the real
/// log's worst row to **120** and the overflow came off the *end*, which is where
/// the size would have been — so on that path the operator would have paid the
/// tail of the error sentence for a number that then fell off the frame anyway.
///
/// ⚠ This is a trade and not a tidy-up: 156 of the 2,156 tool calls on this
/// project's log are unmeasured, so **7.2% of rows carry no size on the feed**.
/// They keep it on the log, and `abcc replay` tallies their characters per tool.
#[test]
fn an_unmeasured_call_spends_its_line_on_the_reason_instead() {
    let text = line_for(Event::ToolCallEnded {
        attempt: AttemptId::at(Seq::new(8)),
        tool: "write_file".to_owned(),
        exit: None,
        elapsed_ms: 0,
        unmeasured: Some(Why::EngineError {
            detail: REAL_500.to_owned(),
        }),
        arguments: None,
        output: Some(Secrets::default().scrub(REAL_500).text),
    });
    assert_one_line(&text);
    assert!(
        !text.contains("bytes back"),
        "the size crowded out the reason the call could not be measured: {text}"
    );
    assert!(
        text.contains("no measurable ending"),
        "the one thing this row exists to say is missing: {text}"
    );
}

/// 🚨 **F748: the one line that says the model was not shown what it was sent.**
///
/// The `model_call_ended` line above it reads perfectly normally — `200`, a
/// finish reason of `tool_calls`, and a smaller token count than one the operator
/// scrolled past several screens ago. Nothing about a cut prompt is visible in a
/// single call, so the feed has to say it in its own line or not at all.
#[test]
fn a_cut_prompt_says_so_and_names_both_numbers() {
    let text = line_for(Event::PromptCut {
        attempt: AttemptId::at(Seq::new(8)),
        reported: 19_181,
        high_water: 36_737,
    });
    assert_one_line(&text);
    for want in ["19181", "36737", "17556"] {
        assert!(
            text.contains(want),
            "a cut prompt's line is missing {want}: {text}"
        );
    }
}
