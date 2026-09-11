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
