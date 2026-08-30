//! The rungs, and the one rule they all obey: **the verdict is the exit status,
//! and the text is only what an operator reads.**
//!
//! ADR-0009 §5 in one sentence — *ask the process, never the prose, and never
//! derive a count that was not printed.* v1 spends 242 lines recovering *did
//! anything run?* by grepping output for the word `passed`, which an all-red
//! pytest run does not contain, so eight real run-endings become three records.
//! The exit status already separates every one of them.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use abcc_core::outcome::{Measurement, Outcome, Reading, Why};
use abcc_engine::child::{Spawn, ToolChild};
use abcc_engine::control::Watch;
use abcc_vcs::{Change, VcsError};

use crate::{paths, why_from_vcs};

/// One rung of the deterministic ladder. Four, and every one of them may refuse,
/// because every one of them is a measurement rather than an opinion
/// (`AttemptPhase::may_refuse`, ADR-0009 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Did the attempt change anything the repository tracks. Free.
    Structural,
    /// The toolchain profile's test command.
    Acceptance,
    /// Deterministic, non-scoring refusal over what the other rungs saw.
    Veto,
    /// The standard the repository declares for itself, when it declares one.
    Standard,
}

impl Rung {
    /// 🚨 **Declaration order, and it is the order the ladder runs and the order
    /// `Report::headline` reports the first red in.** One list, so the two cannot
    /// disagree. See the crate docs for why the veto is third.
    pub const LADDER: &'static [Rung] = &[
        Rung::Structural,
        Rung::Acceptance,
        Rung::Veto,
        Rung::Standard,
    ];

    /// The name on the log and on the console. It is what
    /// `Headline::Red { rung, .. }` names, so it has to be a word an operator can
    /// act on rather than an internal one.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Rung::Structural => "structural",
            Rung::Acceptance => "acceptance",
            Rung::Veto => "veto",
            Rung::Standard => "standard",
        }
    }
}

/// A rung that is not declared for this workspace, answered rather than asserted.
///
/// It is reachable only by a caller that walked past [`crate::Gate::ladder`], and
/// it exists so that doing so produces a sentence instead of a panic.
pub(crate) fn undeclared(rung: Rung) -> Outcome {
    Outcome::Unmeasured {
        rung: rung.name().to_owned(),
        why: Why::NothingToRun {
            detail: format!("{} is not declared for this workspace", rung.name()),
        },
    }
}

/// **The free structural rung: did the attempt change anything.**
///
/// The whole rung is the diff between the two snapshots ADR-0007 already took,
/// at 0.024 s. Its yield is a property of the task's shape rather than a rate to
/// tune — 9 of 12 on repository work against 0 of 29 on clean single-function
/// work — and it has **0 false positives on 609 correct trees**, because a
/// correct change to a repository is a change to a repository.
///
/// 🚨 **It does not try to tell a source file from any other kind, and that is
/// the design.** `git diff` between two snapshots already excludes ignored build
/// output by construction, so `.gitignore` is the repository's own answer to
/// *is this a source file* — and it is a better answer than an extension list,
/// which is exactly the per-language ladder ADR-0008 rejected.
///
/// ⚠ It also owns *empty diff*, which ADR-0008 lists under the veto. One rung
/// owns it rather than two, because a second copy of a rule is a second thing
/// that can drift from the first.
pub(crate) fn structural(sha: &str, diff: Result<&Vec<Change>, &VcsError>) -> Outcome {
    let rung = Rung::Structural.name().to_owned();
    let touched = match diff {
        Err(e) => {
            return Outcome::Unmeasured {
                rung,
                why: why_from_vcs(e),
            };
        }
        Ok(list) => list,
    };
    if touched.is_empty() {
        return Outcome::Measured(Measurement {
            rung,
            sha: sha.to_owned(),
            exit: 1,
            counts: None,
            detail: "the attempt changed no file the repository tracks".to_owned(),
        });
    }
    let named = paths(touched);
    Outcome::Measured(Measurement {
        rung,
        sha: sha.to_owned(),
        exit: 0,
        // 🚨 No counts. `Counts` is `run / passed / failed` and it is about
        // tests; putting a file tally in it would be a number that reads as a
        // test result everywhere it is shown.
        counts: None,
        detail: format!("{} file(s) changed: {}", named.len(), named.join(", ")),
    })
}

/// The two rungs that run a program, which is the only honest probe there is.
///
/// ⚠ **A name that resolves is not a working interpreter** (F312), and a non-zero
/// exit from the wrong program looks exactly like the right one failing (F492).
/// So the failure to start is classified rather than folded into the result:
/// [`Why::CheckerNotOnHost`] and [`Why::SpawnFailed`] are two values because they
/// are two different things to tell an operator.
///
/// 🚨 The child comes from [`ToolChild`] rather than from a second spawner in this
/// crate. That is not reuse for its own sake — it is what gives a rung the same
/// scrubbed environment, the same pipes drained from the first byte (F202) and
/// the same killability from the control thread (F491) that a tool call gets. A
/// gate with its own process code is a gate where those three facts can quietly
/// stop being true.
pub(crate) fn spawned(
    rung: Rung,
    sha: &str,
    root: &Path,
    argv: &[&str],
    reading: Reading,
    watch: &Watch,
    budget: Duration,
) -> Outcome {
    let name = rung.name().to_owned();
    let Some((program, rest)) = argv.split_first() else {
        return Outcome::Unmeasured {
            rung: name,
            why: Why::NothingToRun {
                detail: "the profile named an empty command".to_owned(),
            },
        };
    };
    let spawn = Spawn::new(*program, root)
        .args(rest.iter().map(OsString::from))
        .budget(budget);

    let child = match ToolChild::spawn(&spawn) {
        Ok(child) => child,
        Err(why) => return Outcome::Unmeasured { rung: name, why },
    };
    let finished = child.finish_watching(watch);

    // A timeout or an operator's halt is an absence, not a red suite: the
    // process may still be alive, and `TerminateProcess` hands back exit 1
    // whether or not anything failed.
    if let Some(why) = finished.unmeasured {
        return Outcome::Unmeasured { rung: name, why };
    }
    let Some(exit) = finished.exit else {
        return Outcome::Unmeasured {
            rung: name,
            why: Why::FailedBeforeRunning {
                detail: format!("`{}` ended without an exit status", argv.join(" ")),
            },
        };
    };
    // Both streams, because runners disagree about which one carries the fault:
    // cargo puts diagnostics on stderr and the test summary on stdout, and the
    // evidence a rung reports has to be able to come from either.
    let out = format!("{}\n{}", finished.stdout, finished.stderr);
    reading.classify(&name, sha, exit, &out)
}

/// **The standard rung is a conjunction of commands, and the first refusal ends
/// it.**
///
/// 🚨 **F555 is why this is a loop and not a call.** The rung used to be one
/// command, `cargo clippy --all-targets -- -D warnings`, and a run reached
/// MISSION ACCOMPLISHED on a tree `cargo fmt --check` refuses. Four green rungs
/// and a Judge with no findings said a tree could land that could not land here.
/// A repository's declared standard is not always one program, so the field
/// that holds it is not always one command.
///
/// Two things this owes an operator, and both are why it does not simply return
/// the last outcome:
///
/// * 🚨 **A red says which command.** `standard: exit 1` with clippy's name
///   nowhere near it sends a reader to the compiler for a formatting refusal —
///   F555 read backwards, and the same class as F492's *non-zero exit from the
///   wrong program*.
/// * **A green says what it checked.** The green is the sentence F555 caught
///   lying, so it names every command it stands on rather than asserting a word.
///
/// ⚠ An [`Outcome::Unmeasured`] ends the walk too, and it is **not** a refusal:
/// the rest of the ladder cannot be reported as passed once a command in it
/// could not be run. That is the same rule [`crate::Gate::measure`] applies one
/// level up.
pub(crate) fn standard(
    rung: Rung,
    sha: &str,
    root: &Path,
    commands: &[&[&str]],
    watch: &Watch,
    budget: Duration,
) -> Outcome {
    let name = rung.name().to_owned();
    if commands.is_empty() {
        return Outcome::Unmeasured {
            rung: name,
            why: Why::NothingToRun {
                detail: "the profile declared a standard with no commands".to_owned(),
            },
        };
    }

    let mut passed: Vec<String> = Vec::new();
    for argv in commands {
        let spelled = argv.join(" ");
        // `ExitOnly`: a standard's verdict is its exit status. There is no
        // `test result:` line to read and deriving a count from a linter's prose
        // is what ADR-0009 §5 exists to forbid.
        let outcome = spawned(rung, sha, root, argv, Reading::ExitOnly, watch, budget);
        match outcome {
            Outcome::Measured(m) if m.exit == 0 => passed.push(spelled),
            Outcome::Measured(m) => {
                return Outcome::Measured(Measurement {
                    detail: format!("`{spelled}` refused\n{}", m.detail),
                    ..m
                });
            }
            // Named rather than a wildcard: `Unmeasured` is the arm that ends
            // the walk without refusing, and a `_` here would silently swallow
            // any variant added later.
            unmeasured @ Outcome::Unmeasured { .. } => return unmeasured,
        }
    }

    Outcome::Measured(Measurement {
        rung: name,
        sha: sha.to_owned(),
        exit: 0,
        // No counts: a standard has none, and `Counts` reads as a test result
        // everywhere it is shown.
        counts: None,
        detail: format!("passed: {}", passed.join(", ")),
    })
}
