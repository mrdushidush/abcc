//! **The gate** — the deterministic rungs that may refuse, the conjunction that
//! is the whole of `Accept`, and the one model call that may not.
//!
//! `PLAN.md` §3 calls the Gate *"the part that is actually the product"*, and
//! ADR-0008 says what it is in one line:
//!
//! ```text
//! Accept  ⇔  structural ∧ acceptance ∧ ¬Veto        (and every one of them Measured)
//! Judge   →  a report attached to the attempt, never a term in the conjunction
//! ```
//!
//! Every donor gate is a number and every one of those numbers is a lie in a
//! measurable way — BCF scores a language it has never heard of 8.00 against a
//! gate of 8.00, `result.score || 5` scores a zero as a five, and v1's only
//! binding gate reports pass when the command is absent. So there is no weighted
//! average here, no threshold, and no score anywhere in the type.
//!
//! # 🚨 The conjunction is not a second mechanism — it is `Report::headline`
//!
//! This crate does not implement `∧`. [`abcc_core::outcome::Report::headline`]
//! already returns [`Headline::Green`] **only** when every declared rung produced
//! a measurement and none of them is red, and `Headline::is_pass` is the one
//! function in the workspace that produces a `bool`. So the gate's job is to
//! produce the right [`Outcome`]s, in the right order, and let the type be the
//! conjunction.
//!
//! Two ADR clauses fall out of that rather than being enforced here:
//!
//! * **The Judge cannot vote**, and [`judge`] is now built and still cannot. Its
//!   verdict is an [`abcc_core::outcome::Claim`], which attaches through
//!   `Report::note`, and there is no function anywhere that converts a `Claim`
//!   into an `Outcome`. ADR-0009 §4 is held by the type, so wiring the Judge
//!   into the gate is not something a careless edit can do — there is nothing to
//!   wire it to. ⚠ The rule runs the other way too: **the Judge's own failure is
//!   not the attempt's**, which is `abcc-drive`'s rule 6.
//! * **A rung that could not run does not disappear.** It is `Unmeasured(Why)`,
//!   the headline is `Unverified`, and it lists what was missing. `Green` becomes
//!   rarer and starts meaning something.
//!
//! # The ladder, and why it is in that order
//!
//! [`Rung::LADDER`] is `structural → acceptance → veto → standard`.
//!
//! 1. **Structural first because it is free.** It is the diff between the two
//!    snapshots ADR-0007 already took (0.024 s), and its yield is a property of
//!    the task's shape rather than a rate to tune: 9 of 12 on repository work,
//!    **0 false positives on 609 correct trees**. On this project's own 25 real
//!    attempts it refuses **16**, and each of those 16 costs no cargo at all.
//! 2. **The acceptance test**, which is the rung with the measured yield —
//!    5 of 6 in every cell of the language grid.
//! 3. 🚨 **The veto third rather than last, on purpose.** It reads the
//!    acceptance rung's *absence* and turns the determinate half of it into a
//!    refusal: a tree whose test target will not compile is not *unmeasured*, it
//!    is broken. Putting it after the standard rung would make a tree that does
//!    not build report a lint failure — one failure wearing five hats, which is
//!    the defect `Report::headline`'s own doc names.
//! 4. **The standard**, and only when the repository declares one. See
//!    [`abcc_engine::Standard`] for the evidence that put it here at all, which
//!    is F512 and comes from this repository.
//!
//! 🚨 **The ladder stops at the first refusal and never at an absence.** A red is
//! a decision and there is nothing after it worth spending a cold build on; an
//! absence is not a decision, so the rungs after it still run and the report says
//! everything it could see. This cannot produce a wrong `Green`, because a `Green`
//! requires that nothing refused, and nothing refusing is the case where every
//! rung ran.
//!
//! # What it costs, measured
//!
//! 🚨 **F356 qualifies ADR-0007's shared-`CARGO_TARGET_DIR` ruling: share the
//! build cache for building, never for the gate.** Two trees holding one package
//! name and one shared target directory make cargo print `Fresh`, run *the other
//! tree's* binary and report `ok. 0 passed` at exit 0 — a green test run that
//! measured nothing. Nothing here has to do anything to get that right:
//! `CARGO_TARGET_DIR` is not on [`abcc_engine::child::ENV_ALLOWLIST`], so a rung
//! child never inherits one and cargo falls back to the worktree's own `target/`.
//! The isolation is a consequence of the allowlist rather than a second rule that
//! could be forgotten.
//!
//! The price is a cold build per attempt, and it was measured on this workspace
//! at F512's run 10 rather than guessed: **`cargo test --workspace` 55 s and
//! exit 0 across 44 targets, then `cargo clippy --all-targets -- -D warnings`
//! 13 s and exit 101, for a `target/` of 2.3 GB.** ⚠ That last number is the one
//! Fleet has to plan for: two slots is two of those on the operator's disk.
//!
//! # No error type, on purpose
//!
//! [`Gate::measure`] returns [`Measured`] and cannot fail. Git refusing to answer
//! and cargo not being installed are not errors here — they are
//! `Unmeasured(Why)` on the report, which is exactly what the type is for. **A
//! gate that can fail to answer is a gate with a fourth outcome nobody
//! declared**, and the caller would have to invent a meaning for it.

use std::path::PathBuf;
use std::time::Duration;

use abcc_core::outcome::{Headline, Outcome, Report, Why};
use abcc_engine::Standard;
use abcc_engine::control::Watch;
use abcc_engine::workspace::Toolchain;
use abcc_vcs::{Change, Repo, Sha, VcsError};

pub mod judge;
mod rung;
mod veto;

pub use rung::Rung;

/// How long a rung's child may run before the host stops waiting.
///
/// 🚨 Deliberately larger than [`abcc_engine::workspace::DEFAULT_EXEC_BUDGET`],
/// which is five minutes. A tool call is one command a model asked for against a
/// tree it has already been building in; **a rung is a cold build of the whole
/// workspace**, because the gate may not share a build cache (F356). Measured at
/// 55 s for `cargo test --workspace` here, so ten minutes is roughly an order of
/// magnitude of headroom for a repository larger than this one.
///
/// ⚠ Exceeding it is [`Why::Timeout`] and never an exit status. A timeout folded
/// into "clean" is the worst available lie, because the process may still be
/// alive (F220).
pub const RUNG_BUDGET: Duration = Duration::from_mins(10);

/// What the gate measured, and what it adds up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measured {
    /// Every rung that was reached, measured or not.
    pub report: Report,
    /// The conjunction, computed at the sha the measurements were taken at.
    pub headline: Headline,
    /// The change list — what the structural rung read and what the console
    /// names.
    ///
    /// ⚠ **Not the Judge's pre-image.** `git diff --name-status` answers *which
    /// files*, and ADR-0008's pairwise result is 14 of 14 against *the other
    /// artifact* — the lines, not their filenames. That is
    /// [`abcc_vcs::Repo::patch_between`], which `abcc-drive` asks for separately
    /// when it has a tree worth reviewing. An earlier version of this comment
    /// claimed the two were one thing; a name list is not a pre-image.
    pub changed: Vec<Change>,
}

impl Measured {
    /// Whether an unattended `Accept` is admissible. One function, one `bool`,
    /// and it is `Headline::is_pass` rather than a second opinion.
    #[must_use]
    pub fn accepts(&self) -> bool {
        self.headline.is_pass()
    }

    /// The rung that refused, when one did.
    #[must_use]
    pub fn refused_by(&self) -> Option<&str> {
        match &self.headline {
            Headline::Red { rung, .. } => Some(rung),
            Headline::Green { .. } | Headline::Unverified { .. } => None,
        }
    }
}

/// The deterministic phase, over one worktree and one snapshot pair.
///
/// It holds no `Store` and writes nothing. Recording the rungs is the driver's
/// job — ADR-0009 §7 wants every rung on the log, and the crate that measures
/// should not also be the crate that decides what a log is, for the same reason
/// `abcc-drive` exists.
pub struct Gate<'a> {
    repo: &'a Repo,
    root: PathBuf,
    toolchain: Option<Toolchain>,
    watch: Watch,
    budget: Duration,
}

impl<'a> Gate<'a> {
    /// Open a gate over `root`, using `repo` for the diff.
    ///
    /// 🚨 `repo` must be the repository **as the worktree sees it** when the
    /// snapshots being compared are the attempt's own: `Repo::open` on the
    /// worktree path shares the git directory, so both shas resolve, and the
    /// operator's checkout is never touched.
    #[must_use]
    pub fn open(repo: &'a Repo, root: impl Into<PathBuf>) -> Gate<'a> {
        let root = root.into();
        let toolchain = Toolchain::detect(&root);
        Gate {
            repo,
            root,
            toolchain,
            watch: Watch::detached(),
            budget: RUNG_BUDGET,
        }
    }

    /// Watch this control point, so an operator's urgent verb ends a running
    /// rung instead of waiting out a cold build.
    #[must_use]
    pub fn watching(mut self, watch: Watch) -> Gate<'a> {
        self.watch = watch;
        self
    }

    /// Override the detected profile — an operator's own commands, or a
    /// workspace whose witness file is missing.
    #[must_use]
    pub fn with_toolchain(mut self, toolchain: Toolchain) -> Gate<'a> {
        self.toolchain = Some(toolchain);
        self
    }

    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Gate<'a> {
        self.budget = budget;
        self
    }

    /// The standard this repository declares, if it declares one.
    ///
    /// Two conditions, and they are two because they are two different facts:
    /// the *profile* knows how a standard is spelled for this language, and the
    /// *repository* is what says whether it wants one.
    #[must_use]
    pub fn standard(&self) -> Option<Standard> {
        self.toolchain
            .and_then(|t| t.standard)
            .filter(|s| s.declared_at(&self.root))
    }

    /// The rungs this workspace declares, in ladder order.
    ///
    /// 🚨 A rung that is not declared is **absent**, not missing: it contributes
    /// no `Unmeasured` and does not make the headline `Unverified`. `Green`'s
    /// promise is *every declared rung was measured*, and a workspace that never
    /// asked for a standard has three rungs rather than a failed fourth.
    #[must_use]
    pub fn ladder(&self) -> Vec<Rung> {
        Rung::LADDER
            .iter()
            .copied()
            .filter(|r| *r != Rung::Standard || self.standard().is_some())
            .collect()
    }

    /// Walk the ladder over the change between two snapshots.
    ///
    /// `before` is the attempt's opening checkpoint and `after` its closing one.
    /// Both are snapshots: 🚨 **never diff a snapshot against `HEAD`** — the v1
    /// donor's tree reports 241 phantom files (F329) — and never ask `git status`
    /// what changed (F328).
    #[must_use]
    pub fn measure(&self, before: &Sha, after: &Sha) -> Measured {
        let sha = after.as_str();
        let changed = self.repo.changed_between(before, after);
        let mut report = Report::new();

        for rung in self.ladder() {
            let outcome = match rung {
                Rung::Structural => rung::structural(sha, changed.as_ref()),
                Rung::Veto => veto::measure(sha, &report),
                // The two that run a program. They share an arm because they
                // differ only in which command the profile hands over, which is
                // `run_rung`'s whole job.
                Rung::Acceptance | Rung::Standard => self.run_rung(rung, sha),
            };
            // Read before the move, because a refusal ends the walk and the
            // report owns the outcome from here on.
            let refused = outcome.is_red();
            report.record(outcome);
            if refused {
                break;
            }
        }

        Measured {
            // ADR-0009 §6: a headline computed against a newer sha is
            // `Unverified`, not green. Every measurement here was taken at
            // `after`, so this agrees with `headline()` today — it is written
            // this way because the sha is the thing that makes that true, and
            // saying so is cheaper than a comment nobody re-checks.
            headline: report.headline_at(sha),
            report,
            changed: changed.unwrap_or_default(),
        }
    }

    /// One of the two process rungs: look up its command, run it, read the
    /// ending the profile's way.
    fn run_rung(&self, rung: Rung, sha: &str) -> Outcome {
        let Some(toolchain) = self.toolchain else {
            return Outcome::Unmeasured {
                rung: rung.name().to_owned(),
                // 🚨 The artifact is the **sha**, not the worktree path. The
                // worktree is disposable and is removed a few lines after this
                // is written; a path in a durable log is a path that will not
                // exist when somebody reads it. The sha is the same identifier
                // `Measurement::sha` carries, so a report names one tree
                // throughout.
                why: Why::NoCheckerForArtifact {
                    artifact: sha.to_owned(),
                },
            };
        };
        let (argv, reading) = match rung {
            Rung::Acceptance => (toolchain.test, toolchain.reading),
            Rung::Standard => match self.standard() {
                Some(standard) => (standard.command, abcc_core::outcome::Reading::ExitOnly),
                // Unreachable through `ladder`, and answered rather than
                // asserted: a rung nobody declared has nothing to measure.
                None => return rung::undeclared(rung),
            },
            Rung::Structural | Rung::Veto => return rung::undeclared(rung),
        };
        rung::spawned(
            rung,
            sha,
            &self.root,
            argv,
            reading,
            &self.watch,
            self.budget,
        )
    }
}

/// The `Why` a git failure is, keeping the two things git can do apart.
fn why_from_vcs(error: &VcsError) -> Why {
    match error {
        VcsError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            Why::CheckerNotOnHost {
                binary: "git".to_owned(),
            }
        }
        VcsError::Spawn { source, .. } => Why::SpawnFailed {
            binary: "git".to_owned(),
            os_error: source.to_string(),
        },
        // git ran and could not produce the answer — a bad revision, a tree that
        // is not a repository, output this code cannot read. The checker failed
        // before it could measure anything, which is what this variant says.
        other => Why::FailedBeforeRunning {
            detail: other.to_string(),
        },
    }
}

/// The path a change names, relative to the workspace, for a human to read.
fn paths(changes: &[Change]) -> Vec<&str> {
    changes.iter().map(|c| c.path.as_str()).collect()
}
