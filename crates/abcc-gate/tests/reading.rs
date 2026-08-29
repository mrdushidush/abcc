//! What a rung reports, checked against **real captures** rather than against
//! strings written to match the code.
//!
//! This is the method that caught `counts_from`'s own first draft reading
//! `FAILED (failures=2)` as no failures, and it is the only method that would
//! have caught F517: both defects are invisible when you read the parser and
//! obvious the moment you run it against something a real runner printed.
//!
//! The two captures in `tests/captures/` are from **F512's run 10**, the
//! champion's `--version` implementation whose tests pass and whose only barrier
//! is the standard this repository declares. They are the real output with the
//! `Compiling`/`Checking`/`Finished` progress lines removed — nothing a
//! classifier reads was touched, and the machine-specific paths those lines
//! carried went with them.
//!
//! Process-free: it reads two files and calls two functions.

use abcc_core::outcome::{Counts, Outcome, Reading, evidence};

const GREEN_SUITE: &str = include_str!("captures/cargo-test-green.txt");
const REFUSING_STANDARD: &str = include_str!("captures/cargo-clippy-red.txt");

const SHA: &str = "491f07f2f9";

fn last_nonempty(out: &str) -> &str {
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

// ---------------------------------------------------------------------------
// F517
// ---------------------------------------------------------------------------

/// 🚨 **The defect, asserted about the input rather than about the code.**
///
/// The last line of a *green* run of this workspace's own suite says
/// `test result: ok. 0 passed`. That is F357's sentence — the one the best donor
/// prints for a crate that has no tests — arriving as the human-readable half of
/// a correct measurement, because `cargo test --workspace` prints one summary per
/// target and the last target here is an empty doc-test one.
///
/// This test does not exercise the fix. It pins the fact that made the fix
/// necessary, so that a future simplification back to "the last line" fails here
/// with the reason written down.
#[test]
fn the_last_line_of_a_green_workspace_run_says_zero_passed() {
    let tail = last_nonempty(GREEN_SUITE);
    assert!(
        tail.contains("0 passed"),
        "the capture no longer has the shape this rule is about: {tail}"
    );
    let summaries = GREEN_SUITE
        .lines()
        .filter(|l| l.trim_start().starts_with("test result:"))
        .count();
    assert_eq!(summaries, 44, "one summary per target, and there are many");
}

/// The verdict and the counts are both right, and the sentence beside them no
/// longer contradicts them.
#[test]
fn a_green_workspace_run_measures_every_target_and_says_so() {
    let outcome = Reading::Cargo.classify("acceptance", SHA, 0, GREEN_SUITE);
    let Outcome::Measured(m) = outcome else {
        panic!("a green suite did not measure");
    };
    assert_eq!(m.exit, 0);
    assert_eq!(
        m.counts,
        Some(Counts {
            run: 281,
            passed: 281,
            failed: 0
        }),
        "the counts are the sum of all 44 summaries, not the last one"
    );
    assert_ne!(
        m.detail,
        last_nonempty(GREEN_SUITE),
        "the detail is still the single misleading last line"
    );
    assert!(
        m.detail.lines().count() > 1,
        "one line cannot describe 44 targets: {}",
        m.detail
    );
}

/// 🚨 **rustc and clippy put the fault at the top and the boilerplate at the
/// bottom.** On this capture the line that matters is 12 from the end, and the
/// last line — `could not compile ... due to 1 previous error` — names neither
/// the lint nor the function. A refusal nobody can read is a refusal nobody can
/// fix (F505).
#[test]
fn a_refusing_standard_reports_the_lint_and_not_only_that_it_failed() {
    let outcome = Reading::ExitOnly.classify("standard", SHA, 101, REFUSING_STANDARD);
    assert!(
        outcome.is_red(),
        "a non-zero exit is a refusal, not an absence"
    );
    let Outcome::Measured(m) = outcome else {
        panic!("a refusing standard did not measure");
    };
    assert_eq!(m.exit, 101);
    assert_eq!(m.counts, None, "a linter prints diagnostics, not a tally");
    assert!(
        m.detail.contains("too many lines (101/100)"),
        "the evidence lost the only line that says what to change: {}",
        m.detail
    );
    assert!(
        !last_nonempty(REFUSING_STANDARD).contains("too many lines"),
        "the capture no longer has the shape this rule is about"
    );
}

/// The cap is on a durable record, and it holds on a char boundary rather than a
/// byte one — the diagnostics of a project that writes `🚨` in its own comments
/// go through here.
#[test]
fn the_evidence_is_capped_and_does_not_split_a_character() {
    let long = "error: 🚨 ".repeat(4_000);
    let kept = evidence(&long);
    assert!(kept.chars().count() <= 2_001, "{}", kept.chars().count());
    assert!(kept.ends_with('…'));
}

/// An empty capture is an empty sentence rather than a panic. A checker that
/// printed nothing is a real thing — `cmd /C exit 1` does it.
#[test]
fn silence_reports_as_silence() {
    assert_eq!(evidence(""), "");
    assert_eq!(evidence("   \n\n  \n"), "");
}
