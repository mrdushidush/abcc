//! `Measured | Unmeasured(Why)` — the one type that must not be added later.
//!
//! ADR-0009, and `PLAN.md` ground rule 4. This module is carried forward from
//! W6 item 8's spike (`research/spikes/w6-honesty/report/`, F358), which was our
//! own code rather than a donor's, so the rewrite ruling does not touch it. Three
//! things changed on the way in: `rung` became a `String` because a replayed log
//! has to deserialize into an owned value, a [`Measurement`] now names the sha it
//! was taken at (ADR-0009 §6), and every type serializes, because ADR-0009 §7
//! says every rung — measured or not — is an event on the log.
//!
//! The family's vocabulary for *"I did not measure that"* is the same word as
//! *"I measured it and it was fine"*. That is the design failure this type
//! removes, and it is not that the donors lie:
//!
//! * v1's dispatcher is gated on the word `passed`, which an all-red pytest run
//!   does not contain, so eight real run-endings become three records — *every
//!   test failed* is indistinguishable from *there were no tests*. The exit status
//!   separates all of them and no donor reads one (F350).
//! * v1's `UNCERTAIN` exists, is constructed once, and sets `success=True` in the
//!   same literal.
//! * The best of the three still returns `Some(true)` with the line
//!   "tests: 0 passed" for a cargo crate that has none (F357).
//!
//! Three rules, all enforced by the type rather than by a call site.
//!
//! 1. **There is no `bool`.** A rung either produced a measurement the host
//!    watched, or it did not, and if it did not, [`Why`] carries the reason.
//!    "There is no checker for this artifact", "the checker is not on this
//!    machine" and "the checker ran and found nothing to run" are three values
//!    because they are three different things to tell an operator.
//! 2. **A pass is a statement about coverage.** [`Report::headline`] returns
//!    [`Headline::Green`] only when every rung the report declared produced a
//!    measurement. `Green` becomes rarer and starts meaning something.
//! 3. **What the model said is a different type from what the host saw.** A
//!    [`Claim`] is shown to the operator and there is no function anywhere that
//!    turns one into an [`Outcome`]. The claims are useful, interesting and often
//!    true — they are simply not evidence.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What the host watched a checker do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measurement {
    /// Which rung this is — the name the console shows.
    pub rung: String,
    /// The commit sha the tree was at when this was taken (ADR-0007's snapshot,
    /// 0.16 s). Both of F354's failure modes are invisible in a truthful
    /// sentence, and this closes the first of them: 6 of 81 measured trees no
    /// longer existed, because the edit landed 1.7–3.0 s after the test binary.
    pub sha: String,
    /// The process's exit status, as the OS reported it.
    pub exit: i32,
    /// Counts, when the runner printed them in a form this profile can read.
    /// `None` means the process ran and the counts were not recoverable, which is
    /// a different thing from zero.
    pub counts: Option<Counts>,
    /// Kept for the console and for the next attempt's prompt, never parsed for a
    /// verdict.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub run: u32,
    pub passed: u32,
    pub failed: u32,
}

/// Why a rung produced no measurement. Every variant is a sentence an operator
/// can act on, and none of them is `false`.
///
/// ADR-0009 §7: this is also W3's failure class. `BudgetExhausted`, `Timeout` and
/// `Cancelled` were already members of that enum, and the three checker-absence
/// reasons joined them, so the report is a projection of the log rather than a
/// second source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum Why {
    /// There is no checker for this kind of artifact — a `.md` file has no test
    /// suite, and saying so is honest.
    NoCheckerForArtifact { artifact: String },
    /// The profile named a binary that is not on this host. Probe by executing: a
    /// name that resolves is not a working interpreter (F312).
    CheckerNotOnHost { binary: String },
    /// The binary is there and the process would not start.
    SpawnFailed { binary: String, os_error: String },
    /// The checker ran to completion and there was nothing for it to run. This is
    /// the case v1 records as success.
    NothingToRun { detail: String },
    /// The checker failed before it could run anything — a collection error, a
    /// broken import, a harness that does not compile.
    FailedBeforeRunning { detail: String },
    /// The host stopped waiting. A timeout folded into "clean" is the worst
    /// available lie, because the process may still be alive (F220).
    Timeout { after_ms: u64 },
    /// A budget ran out. Returning `Ok(())` here is forbidden by this type
    /// (W11 F296): exhaustion is neither pass nor fail.
    BudgetExhausted { which: String },
    /// The operator stopped it. Still not a failure of the work.
    Cancelled { by: String },
    /// The model stopped at the token cap with nothing in the payload. The 17 of
    /// 57 judge calls lost this way are absences, not failures, and this is the
    /// `Uncertain` `PLAN.md` §5 requires from day one.
    TruncatedAtCap { budget: u32 },
    /// The measurement exists but was taken at a sha the tree has since left.
    /// See [`Report::headline_at`].
    StaleMeasurement { taken_at: String, now: String },
    /// 🚨 The role's ceiling refused the call, so nothing ran and there is
    /// nothing to measure.
    ///
    /// A denial is a **normal outcome**, not a fault: ADR-0014's control is
    /// denying the class, and a class denied silently is a class nobody can
    /// audit. It is a member of this enum for the same reason the other ten are
    /// — it is a sentence an operator can act on, and it is not `false`.
    Denied {
        role: String,
        tool: String,
        ceiling: String,
    },
    /// The turn loop itself broke — a malformed response, a provider error, a
    /// tool layer that could not answer. Attempt-level rather than rung-level,
    /// and it is here because ADR-0009 §7 makes this enum W3's failure class too.
    EngineError { detail: String },
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::NoCheckerForArtifact { artifact } => write!(f, "no checker for {artifact}"),
            Why::CheckerNotOnHost { binary } => write!(f, "{binary} is not on this machine"),
            Why::SpawnFailed { binary, os_error } => {
                write!(f, "{binary} would not start: {os_error}")
            }
            Why::NothingToRun { detail } => write!(f, "ran and found nothing to run ({detail})"),
            Why::FailedBeforeRunning { detail } => {
                write!(f, "failed before running anything ({detail})")
            }
            Why::Timeout { after_ms } => write!(f, "still running after {after_ms} ms"),
            Why::BudgetExhausted { which } => write!(f, "{which} budget exhausted"),
            Why::Cancelled { by } => write!(f, "cancelled by {by}"),
            Why::TruncatedAtCap { budget } => {
                write!(f, "stopped at the {budget}-token cap with an empty payload")
            }
            Why::StaleMeasurement { taken_at, now } => {
                write!(f, "measured at {taken_at}, tree is now at {now}")
            }
            Why::Denied {
                role,
                tool,
                ceiling,
            } => write!(f, "{role} may not use {tool} (capped at {ceiling})"),
            Why::EngineError { detail } => write!(f, "the turn loop failed: {detail}"),
        }
    }
}

/// A rung's result. Note what is missing: a variant meaning "fine".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Measured(Measurement),
    Unmeasured { rung: String, why: Why },
}

impl Outcome {
    /// Green means: a measurement exists, and it says nothing failed.
    #[must_use]
    pub fn is_green(&self) -> bool {
        match self {
            Outcome::Measured(m) => m.exit == 0,
            Outcome::Unmeasured { .. } => false,
        }
    }

    /// Red means: a measurement exists, and it says something failed. An absent
    /// measurement is not red either — that is the whole point of the type.
    #[must_use]
    pub fn is_red(&self) -> bool {
        match self {
            Outcome::Measured(m) => m.exit != 0,
            Outcome::Unmeasured { .. } => false,
        }
    }

    #[must_use]
    pub fn rung(&self) -> &str {
        match self {
            Outcome::Measured(m) => &m.rung,
            Outcome::Unmeasured { rung, .. } => rung,
        }
    }
}

/// Something the model said. It is shown to the operator and it is never
/// evidence. There is deliberately no `From<Claim> for Outcome`.
///
/// v1's defect is not that its model lied — on 98 unimpeded attempts the champion
/// asserted the tests pass 81 times and a test really ran 81 of 81. The defect is
/// that `test_results` is a string a model writes and a gate reads. What prose
/// cannot carry is scope and time, not truth: 14 of those 81 true, measured,
/// green claims sat on trees a hidden gate failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub by: String,
    pub text: String,
}

/// What the console shows and what the gate consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "headline", rename_all = "snake_case")]
pub enum Headline {
    /// Every declared rung produced a measurement and all of them were green.
    Green { rungs: usize },
    /// A measurement says something failed. The first one, in rung order.
    Red { rung: String, detail: String },
    /// Not enough was measured to say either way, and here is exactly what was
    /// missing. This is the outcome the donors do not have.
    Unverified { missing: Vec<(String, Why)> },
}

impl Headline {
    /// The only place in the crate where a verdict becomes a Boolean, and it is
    /// deliberately not `is_ok`: `Unverified` is not a pass and not a failure.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Headline::Green { .. })
    }
}

impl fmt::Display for Headline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Headline::Green { rungs } => write!(f, "checks passed ({rungs} measured)"),
            Headline::Red { rung, detail } => write!(f, "{rung} failed: {detail}"),
            Headline::Unverified { missing } => {
                write!(f, "unverified — ")?;
                for (i, (rung, why)) in missing.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{rung}: {why}")?;
                }
                Ok(())
            }
        }
    }
}

/// The record a finished unit hands back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    outcomes: Vec<Outcome>,
    claims: Vec<Claim>,
}

impl Report {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, outcome: Outcome) -> &mut Self {
        self.outcomes.push(outcome);
        self
    }

    /// Attach what the model said. It changes no verdict; see [`Report::headline`].
    pub fn note(&mut self, claim: Claim) -> &mut Self {
        self.claims.push(claim);
        self
    }

    #[must_use]
    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    #[must_use]
    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    /// How much of what was attempted actually produced a measurement — the
    /// coverage fraction, at the reporting layer.
    #[must_use]
    pub fn measured_fraction(&self) -> (usize, usize) {
        let measured = self
            .outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::Measured(_)))
            .count();
        (measured, self.outcomes.len())
    }

    /// Rung order is declaration order and the first red wins: in Rust a type
    /// error takes the suite down with it, so parallel verdicts report one failure
    /// wearing five hats.
    #[must_use]
    pub fn headline(&self) -> Headline {
        if let Some(Outcome::Measured(m)) = self.outcomes.iter().find(|o| o.is_red()) {
            return Headline::Red {
                rung: m.rung.clone(),
                detail: m.detail.clone(),
            };
        }
        let missing: Vec<(String, Why)> = self
            .outcomes
            .iter()
            .filter_map(|o| match o {
                Outcome::Unmeasured { rung, why } => Some((rung.clone(), why.clone())),
                Outcome::Measured(_) => None,
            })
            .collect();
        if !missing.is_empty() || self.outcomes.is_empty() {
            return Headline::Unverified { missing };
        }
        Headline::Green {
            rungs: self.outcomes.len(),
        }
    }

    /// The headline as of a tree at `sha`. ADR-0009 §6: a headline computed
    /// against a newer sha is `Unverified`, not green.
    ///
    /// This is the mechanical fix for the failure mode where the tree moved after
    /// the measurement. The other one — the measurement was of the wrong thing —
    /// is not fixable by honesty at all, and the report's job there is only to
    /// stop implying a scope it does not have.
    #[must_use]
    pub fn headline_at(&self, sha: &str) -> Headline {
        let stale: Vec<(String, Why)> = self
            .outcomes
            .iter()
            .filter_map(|o| match o {
                Outcome::Measured(m) if m.sha != sha => Some((
                    m.rung.clone(),
                    Why::StaleMeasurement {
                        taken_at: m.sha.clone(),
                        now: sha.to_owned(),
                    },
                )),
                _ => None,
            })
            .collect();
        if stale.is_empty() {
            return self.headline();
        }
        match self.headline() {
            // A red measurement at an old sha is still a red measurement: the
            // fault was real, and staleness does not un-observe it.
            red @ Headline::Red { .. } => red,
            Headline::Green { .. } => Headline::Unverified { missing: stale },
            Headline::Unverified { mut missing } => {
                missing.extend(stale);
                Headline::Unverified { missing }
            }
        }
    }
}

/// The classifier the donors needed.
///
/// Both runners answer *"did anything run?"* in the exit status — pytest and
/// `python -m unittest` both use **5** for "no tests collected", **2** for
/// "interrupted before running", **1** for "ran and something failed" and **0**
/// for "ran and all passed". v1 recovers the same question by grepping the output
/// for the word `passed`, which an all-red run does not contain, and spends 242
/// lines failing at it.
///
/// Counts are read from the text where the text has them, and their absence is
/// `None` rather than zero.
#[must_use]
pub fn classify_python_tests(rung: &str, sha: &str, exit: i32, out: &str) -> Outcome {
    let tail = last_nonempty(out);
    match exit {
        5 => Outcome::Unmeasured {
            rung: rung.to_owned(),
            why: Why::NothingToRun { detail: tail },
        },
        2..=4 => Outcome::Unmeasured {
            rung: rung.to_owned(),
            why: Why::FailedBeforeRunning { detail: tail },
        },
        _ => Outcome::Measured(Measurement {
            rung: rung.to_owned(),
            sha: sha.to_owned(),
            exit,
            counts: counts_from(out),
            detail: tail,
        }),
    }
}

/// The same question for cargo, which answers it differently.
///
/// `cargo test` has no exit code for *nothing to run*: a crate with no tests
/// exits **0** and prints `test result: ok. 0 passed`, which is why the best
/// donor's `classify_tests` returns `Some(true)` for it. And it exits **101**
/// both for a red suite and for a test target that does not compile, so the exit
/// status alone cannot separate those either.
///
/// What does separate them is whether cargo printed a `test result:` line at all,
/// and what the numbers on it are:
///
/// * no `test result:` line and a non-zero exit — the test target never ran.
/// * a line whose totals are all zero — the target ran and there was nothing in
///   it, which is not a pass.
/// * anything else — a measurement, green or red on the exit status.
#[must_use]
pub fn classify_cargo_tests(rung: &str, sha: &str, exit: i32, out: &str) -> Outcome {
    let tail = last_nonempty(out);
    let summaries: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("test result:"))
        .collect();

    if summaries.is_empty() {
        return Outcome::Unmeasured {
            rung: rung.to_owned(),
            why: Why::FailedBeforeRunning { detail: tail },
        };
    }
    let counts = counts_from(&summaries.join("\n"));
    let nothing = counts.is_none_or(|c| c.run == 0);
    if exit == 0 && nothing {
        return Outcome::Unmeasured {
            rung: rung.to_owned(),
            why: Why::NothingToRun {
                detail: summaries[0].to_owned(),
            },
        };
    }
    Outcome::Measured(Measurement {
        rung: rung.to_owned(),
        sha: sha.to_owned(),
        exit,
        counts,
        detail: tail,
    })
}

fn last_nonempty(out: &str) -> String {
    out.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_owned()
}

/// Order-free count extraction, with one rule: **never return a number that was
/// not derivable from what the runner printed.**
///
/// Two shapes have to be read. pytest prints the parts, in whatever order it
/// likes — `2 failed, 1 passed in 0.30s` — which is why v1's regex, requiring
/// `passed` first, reports 2 run and 0 passed for that line. unittest prints the
/// total on one line (`Ran 2 tests in 0.001s`) and the breakdown on another, as
/// `OK` or `FAILED (failures=2, errors=1)`.
///
/// The first draft of this function derived `passed = total - failed` for
/// unittest and read `failures=2` as no failures, because the count is inside a
/// `key=value` token and not a `<n> <word>` pair. It therefore reported *2 of 2
/// passed* on a run where both tests failed — the same invented number this
/// module is about, in the code written to prevent it. It was caught by running
/// it against a real capture, not by reading it.
fn counts_from(out: &str) -> Option<Counts> {
    let mut passed: Option<u32> = None;
    let mut failed: Option<u32> = None;
    let mut errors: Option<u32> = None;
    let mut skipped = 0u32;
    let mut ran: Option<u32> = None;
    let mut unittest_ok = false;
    let mut unittest_failed = false;

    for line in out.lines() {
        let l = line.trim();
        if l == "OK" || l.starts_with("OK (") {
            unittest_ok = true;
        }
        if l.starts_with("FAILED (") {
            unittest_failed = true;
        }

        // `key=value` tokens: unittest's failures=2, errors=1, skipped=3.
        for tok in l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '=')) {
            let Some((key, value)) = tok.split_once('=') else {
                continue;
            };
            let Ok(n) = value.parse::<u32>() else {
                continue;
            };
            match key {
                "failures" => failed = Some(failed.unwrap_or(0) + n),
                "errors" => errors = Some(errors.unwrap_or(0) + n),
                "skipped" => skipped += n,
                _ => {}
            }
        }

        // `<n> <word>` pairs: pytest's summary line and unittest's "Ran N tests".
        let words: Vec<&str> = l.split_whitespace().collect();
        for (i, pair) in words.windows(2).enumerate() {
            let digits: String = pair[0].chars().filter(char::is_ascii_digit).collect();
            let Ok(n) = digits.parse::<u32>() else {
                continue;
            };
            let word: String = pair[1].chars().filter(char::is_ascii_alphabetic).collect();
            match word.as_str() {
                "passed" => passed = Some(passed.unwrap_or(0) + n),
                "failed" => failed = Some(failed.unwrap_or(0) + n),
                "error" | "errors" => errors = Some(errors.unwrap_or(0) + n),
                "skipped" => skipped += n,
                "tests" | "test" if i > 0 && words[i - 1] == "Ran" => ran = Some(n),
                _ => {}
            }
        }
    }

    let bad = failed.unwrap_or(0) + errors.unwrap_or(0);
    match ran {
        // unittest: the total is printed, the breakdown is a word.
        Some(total) if unittest_ok && failed.is_none() && errors.is_none() => Some(Counts {
            run: total,
            passed: total.saturating_sub(skipped),
            failed: 0,
        }),
        Some(total) if unittest_failed && (failed.is_some() || errors.is_some()) => Some(Counts {
            run: total,
            passed: total.saturating_sub(bad + skipped),
            failed: bad,
        }),
        // It said how many ran and nothing this profile can read about how they
        // went. That is a measurement with no counts, not a green one.
        Some(_) => None,
        // pytest: the parts are printed and the total is their sum.
        None if passed.is_none() && failed.is_none() && errors.is_none() => None,
        None => {
            let p = passed.unwrap_or(0);
            Some(Counts {
                run: p + bad + skipped,
                passed: p,
                failed: bad,
            })
        }
    }
}
