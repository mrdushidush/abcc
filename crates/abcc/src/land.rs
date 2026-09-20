//! The ninth verb, and the one W13's M1 is defined in: **the work leaves the
//! worktree and becomes a commit on the operator's branch.**
//!
//! M1 is *ABCC 2.0 takes a task from its own tracker and runs its own pipeline
//! end to end unattended — plan, worktree, edit, gate — with the human's only
//! act being merge.* Every clause of that shipped in Skeleton except the last
//! one, and the last one had **no act to perform**: `abcc accept` is
//! `Aborted { CompletedByOperator }`, which is deliberately *a person finished
//! this by hand* and deliberately not a merge, and nothing else in the binary
//! writes to a branch at all.
//!
//! # 🚨 The git side was already finished, and that is why this module is small
//!
//! [`abcc_vcs::Repo::checkpoint`] is `commit-tree -p HEAD` plus `update-ref`,
//! not a stash — so **every snapshot this project ever took is a real commit
//! that is still in the object store**, held there by its ref (F330). An
//! attempt's pair is `before` and `after`, and `after`'s parent is `before`, so
//! what a landing has to do is apply one diff. Nothing here invents a
//! representation of the change: it asks for the same
//! [`abcc_vcs::Repo::patch_between`] the Judge was shown, so **what lands is
//! what was reviewed**.
//!
//! What was actually missing was the *reading* — nothing connected *the gate
//! went green on attempt N* to *here is the sha, here is the diff, here is the
//! command that lands it*, and nothing wrote a row when it happened.
//!
//! # 🚨 The entitlement is `Headline::Green`, and it is not a second opinion
//!
//! [`landing`] rebuilds an [`abcc_core::outcome::Report`] from the attempt's own
//! `RungRecorded` rows and asks [`Report::headline_at`]. That is the same
//! function `abcc-drive` asks before it is allowed to say `Accomplished`, and
//! asking it again here — rather than counting green rungs locally — is F392:
//! two functions answering one question is two answers waiting to disagree.
//!
//! `headline_at` rather than `headline` buys ADR-0009 §6 for free: a rung
//! measured at a *different* sha from the one being landed is
//! `Why::StaleMeasurement` and the headline is `Unverified`, not green. A gate
//! whose rungs do not all name one tree has not measured one tree.
//!
//! ⚠ **`Accomplished` is not the test, and must not become it.** The state says
//! a gate passed; it is reachable on a log whose checkpoints have since been
//! pruned, and F730's lesson is that a state is a claim about the past while a
//! landing is an act on the present. The rungs and the shas are what this reads.
//!
//! # ⚠ What it refuses, and why each refusal is the honest one
//!
//! * **A dirty checkout.** Applying a patch over uncommitted work is how the
//!   operator loses it, and [`abcc_vcs::Repo::is_clean`] errs toward refusing.
//! * **A patch that will not apply.** `--3way` reconstructs the pre-image from
//!   the blobs, so an old pair still lands — the oldest green pair on this
//!   project's own log is 58 commits behind. When it genuinely conflicts, git is
//!   all-or-nothing and the refusal carries git's own sentence.
//! * **A binary hunk.** `patch_between` is the Judge's pre-image and carries no
//!   `--binary`, so a binary change is a line saying the files differ. Applying
//!   that would land *part* of a change, which is the one outcome worse than
//!   landing none of it.
//! * **An empty diff.** A green gate over a tree that changed nothing is a real
//!   thing (F730's `div_ceil` test could not have failed either) and there is no
//!   commit to make.
//!
//! # ⚠ This is an operator verb, and the fleet must never call it
//!
//! OQ-W13-4 — *does the agent get commit rights, ever?* — is open, and v1's
//! `CLAUDETTE.md` says never. A landing reached from `abcc fleet` would answer
//! that question by accident, in a direction nobody ruled on. So it lives here
//! beside `accept` and `take`, it is reachable only from a word the operator
//! types, and [`abcc_vcs::Repo::commit`] is the only function in that crate that
//! moves a branch.
//!
//! ⚠ **It is not `abcc review`'s problem, and it does not solve it.** This
//! writes who landed what. The minutes a human spent reading it are a different
//! measurement that only a human can supply, and the two are kept apart for the
//! reason F727 gives.

use std::collections::HashMap;
use std::io::Write;

use abcc_core::event::Event;
use abcc_core::outcome::{Headline, Outcome, Report};
use abcc_core::replay::{AttemptTrace, Replay, TaskTrace};
use abcc_core::seq::{AttemptId, Seq, TaskId};
use abcc_vcs::Sha;

use crate::ops::{self, open_log};
use crate::{AppError, Invocation, cli, operator};

/// What a landing would take, and the pair it would take it between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Landing {
    pub attempt: AttemptId,
    /// The checkpoint the attempt opened on.
    pub from: String,
    /// The checkpoint it closed on, which is the sha every rung was measured at.
    pub to: String,
    /// How many rungs were green — `Headline::Green { rungs }`, kept because it
    /// is the whole of the entitlement.
    pub rungs: usize,
}

/// The shas of every checkpoint on the log, by the seq that is its id.
///
/// ⚠ Built from the raw events rather than from [`Replay`], because a
/// checkpoint's sha is a *field* and the fold keeps only the id — which is the
/// right thing for it to keep and the wrong thing to land from.
pub(crate) fn checkpoints(log: &[abcc_core::event::Logged]) -> HashMap<Seq, String> {
    log.iter()
        .filter_map(|l| match &l.event {
            Event::CheckpointTaken { sha, .. } => Some((l.seq, sha.clone())),
            _ => None,
        })
        .collect()
}

/// Pick the attempt to land, or say why there is none.
///
/// 🚨 **Newest first, and the first green one wins.** A task that was retried
/// has several attempts and at most one of them is the work: the later attempt
/// forked from the earlier one's checkpoint (`Cause`), so landing an older green
/// attempt would land a tree the project has already moved past.
pub(crate) fn landing(
    trace: &TaskTrace,
    checkpoints: &HashMap<Seq, String>,
) -> Result<Landing, String> {
    if trace.attempts.is_empty() {
        return Err(format!(
            "{} has never run, so there is nothing to land — `abcc run --task {}` first",
            trace.id, trace.id
        ));
    }
    if let Some(landed) = trace.attempts.iter().rev().find_map(|a| a.landed.as_ref()) {
        return Err(format!(
            "{} was already landed as {landed} — landing it twice would be two commits of one \
             change, and the ladder's unit is the change",
            trace.id
        ));
    }

    let mut seen = Vec::new();
    for attempt in trace.attempts.iter().rev() {
        match green(attempt) {
            Ok((to, rungs)) => {
                let from = opening(attempt, checkpoints).ok_or_else(|| {
                    format!(
                        "{}'s gate was green at {to} but the log does not say which checkpoint it \
                         opened on, so there is no pair to take a diff between",
                        attempt.id
                    )
                })?;
                return Ok(Landing {
                    attempt: attempt.id,
                    from,
                    to,
                    rungs,
                });
            }
            Err(why) => seen.push(format!("{} {why}", attempt.id)),
        }
    }
    Err(format!(
        "no attempt of {} has a green gate, so nothing here is entitled to land: {}",
        trace.id,
        seen.join("; ")
    ))
}

/// The sha this attempt's rungs were measured at, if every declared rung was
/// measured there and every one of them was green.
fn green(attempt: &AttemptTrace) -> Result<(String, usize), String> {
    let Some(sha) = measured_at(&attempt.rungs) else {
        return Err("recorded no measured rung".to_owned());
    };
    let mut report = Report::new();
    for outcome in &attempt.rungs {
        report.record(outcome.clone());
    }
    match report.headline_at(&sha) {
        Headline::Green { rungs } => Ok((sha, rungs)),
        // The headline's own words, so the refusal says what the gate said
        // rather than a paraphrase of it.
        other => Err(format!("is {other}")),
    }
}

/// The sha the first measured rung was taken at.
///
/// ⚠ Deliberately the *first* and not a check that they agree: disagreement is
/// [`Report::headline_at`]'s to report, as `Why::StaleMeasurement`, and a second
/// check here would be a second answer to that question.
fn measured_at(rungs: &[Outcome]) -> Option<String> {
    rungs.iter().find_map(|o| match o {
        Outcome::Measured(m) => Some(m.sha.clone()),
        Outcome::Unmeasured { .. } => None,
    })
}

/// The sha of the checkpoint an attempt opened on.
fn opening(attempt: &AttemptTrace, checkpoints: &HashMap<Seq, String>) -> Option<String> {
    let id = attempt.checkpoint_from?;
    checkpoints.get(&id.born()).cloned()
}

/// Land a green attempt's work on the operator's branch.
///
/// # Errors
///
/// [`AppError::Refused`] if the log never created the task, if no attempt of it
/// is entitled to land, or if the checkout is not clean; [`AppError::Vcs`] if
/// git will not apply the patch or make the commit.
pub fn land(
    invocation: &Invocation,
    task: cli::TaskRef,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let mut store = open_log(&ground.home)?;
    let log = crate::fun::read_all(&store)?;
    let replay = Replay::over(&log);
    let id = TaskId::at(Seq::new(task.0));
    let trace = replay.task(id).ok_or_else(|| {
        AppError::Refused(format!(
            "the log never created {id} — try `abcc board` for what it did create"
        ))
    })?;

    let landing = landing(trace, &checkpoints(&log)).map_err(AppError::Refused)?;

    // Before the patch, because a refusal after a partial apply is the one
    // failure that costs the operator work rather than a retry.
    if !ground.repo.is_clean()? {
        return Err(AppError::Refused(format!(
            "{} has uncommitted changes. A landing applies a patch to this tree, so it refuses \
             rather than mix somebody's work into an attempt's — commit or stash first.",
            ground.repo.root().display()
        )));
    }

    let from = sha(&landing.from)?;
    let to = sha(&landing.to)?;
    let patch = ground.repo.patch_between(&from, &to)?;
    refuse_unlandable(&patch)?;

    // 🚨 **`git apply --3way` is not all-or-nothing, and finding that out is
    // what this line is.** The plain applier is: it stages nothing unless the
    // whole patch applies. The three-way fallback is a *merge*, so a hunk it
    // cannot reconcile is written to the file with conflict markers and left as
    // an unmerged index entry — `Applied patch to 'src.rs' with conflicts. U
    // src.rs`, exit 1. A verb that returned that error and stopped would hand
    // the operator a checkout in mid-merge for a landing that did not happen.
    //
    // The undo is exact because [`Repo::is_clean`] was asked first: there is
    // nothing in this tree that is not in `HEAD`, so resetting to `HEAD` can
    // only remove what the failed apply just wrote.
    if let Err(refused) = ground.repo.apply(&patch) {
        let head = ground.repo.head()?;
        ground.repo.restore(&head)?;
        return Err(refused.into());
    }
    let change = ground.repo.commit(&message(trace, &landing))?;

    store.append(Event::ChangeLanded {
        task: id,
        attempt: landing.attempt,
        change: change.as_str().to_owned(),
        from: landing.from.clone(),
        to: landing.to.clone(),
        rungs: landing.rungs,
    })?;

    writeln!(
        out,
        "landed  {}  {}  from {} ({} rung(s) green)",
        change.short(),
        landing.attempt,
        &landing.to[..7],
        landing.rungs
    )?;
    // 🚨 The whole reason a landing names a sha. W13's ladder is measured in
    // human review minutes per merged change and it has never had a row; this
    // is the line that makes the next one one command.
    writeln!(
        out,
        "Now the measurement: `abcc review {} <minutes>`, and `--boundary` if it crossed one.\n\
         The minutes are a person's — an agent recording its own is the one number that \
         cannot be faked into meaning something.",
        change.as_str()
    )?;
    Ok(())
}

/// The checkpoint an attempt closed on, for the attempts a landing will not
/// take.
///
/// 🚨 **This is the fallback and never the first answer.** A green attempt's
/// `to` is the sha its rungs were *measured at*, which is what [`landing`]
/// returns and what [`land`] applies. Reading a second answer off the
/// checkpoints and showing *that* would be F392 exactly — two functions
/// answering one question are two answers waiting to disagree, and the question
/// here is *what would land*. This exists for the attempts [`landing`] refuses,
/// where there are no measured rungs to read a sha off and the snapshot the run
/// took on its way out is the only record of what the model wrote.
///
/// ⚠ Filtered by task as well as by span. Two slots overlap, and a seq range
/// alone would hand back the neighbouring attempt's checkpoint.
///
/// ⚠ **An open upper bound is deliberate.** `ended` is `None` for an attempt
/// still flying and for one a crash abandoned, and those are the two states an
/// operator most wants to look inside. Requiring an ending would refuse them
/// the one view that could say what happened.
fn closing(
    log: &[abcc_core::event::Logged],
    attempt: &AttemptTrace,
    task: TaskId,
) -> Option<String> {
    log.iter()
        .filter(|l| l.seq > attempt.started && attempt.ended.is_none_or(|e| l.seq <= e))
        .filter_map(|l| match &l.event {
            Event::CheckpointTaken { task: t, sha, .. } if *t == task => Some(sha.clone()),
            _ => None,
        })
        .next_back()
}

/// What an attempt wrote, as a patch.
///
/// 🚨 **The verb that makes `abcc review` mean something.** W13 scores this
/// project in human review minutes per merged change, and until this existed
/// the operator was asked for that number over a diff no command would print —
/// so the five rows on the log claim 480 seconds of reading that the wall clock
/// between each landing and its own review row leaves no room for. A metric
/// nobody can perform is not a thin measurement, it is a fabricated one.
///
/// ⚠ **It shows red attempts too, and that is most of the value.** 88 of 125
/// attempts on this log never reached a gradeable artifact; `abcc replay` says
/// *which rung stopped it* and could never say *what it had written when it
/// stopped*.
///
/// # Errors
///
/// [`AppError::Refused`] if the log never created the task, if it has never
/// run, or if there is no checkpoint pair to take a diff between;
/// [`AppError::Vcs`] if git will not produce the diff.
pub fn diff(
    invocation: &Invocation,
    task: cli::TaskRef,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let store = open_log(&ground.home)?;
    let log = crate::fun::read_all(&store)?;
    let replay = Replay::over(&log);
    let id = TaskId::at(Seq::new(task.0));
    let trace = replay.task(id).ok_or_else(|| {
        AppError::Refused(format!(
            "the log never created {id} — try `abcc board` for what it did create"
        ))
    })?;

    let checkpoints = checkpoints(&log);
    // 🚨 [`landing`] first, always. When it answers, what prints is the pair a
    // landing would take — from the same function, so the operator is reading
    // the bytes that will land rather than a second opinion about them.
    let (from, to, standing) = match landing(trace, &checkpoints) {
        Ok(l) => (
            l.from,
            l.to,
            format!(
                "{} — {} rung(s) green. This is exactly what `abcc land {id}` would apply.",
                l.attempt, l.rungs
            ),
        ),
        Err(why) => {
            let attempt = trace
                .attempts
                .last()
                .ok_or_else(|| AppError::Refused(why.clone()))?;
            let from = opening(attempt, &checkpoints).ok_or_else(|| {
                AppError::Refused(format!(
                    "{} does not say which checkpoint it opened on, so there is no pair to take a \
                     diff between",
                    attempt.id
                ))
            })?;
            let to = closing(&log, attempt, id).ok_or_else(|| {
                AppError::Refused(format!(
                    "{} took no checkpoint on its way out, so nothing recorded what it wrote",
                    attempt.id
                ))
            })?;
            (
                from,
                to,
                format!(
                    "{} — will not land: {}. This is what it wrote anyway.",
                    attempt.id,
                    first_line(&why)
                ),
            )
        }
    };

    let patch = ground.repo.patch_between(&sha(&from)?, &sha(&to)?)?;

    writeln!(out, "{}  {}", trace.id, trace.title)?;
    writeln!(out, "{standing}")?;
    writeln!(out, "checkpoints {}..{}", &from[..7], &to[..7])?;
    writeln!(out)?;
    if patch.trim().is_empty() {
        // ⚠ Not "nothing to see". An empty pair is the structural rung's own
        // finding — the attempt ran and touched nothing tracked — and a reader
        // who mistakes it for a missing diff will go looking for a bug here.
        writeln!(out, "the attempt changed no file the repository tracks")?;
        return Ok(());
    }
    write!(out, "{patch}")?;
    Ok(())
}

/// One line of a refusal, for a header that is one line.
///
/// ⚠ **A rung's refusal carries the tool's captured output**, and `cargo test`
/// announces every binary it ran before it says what failed. Interpolating that
/// whole into a header puts `Running tests\cli.rs` in the middle of a sentence
/// about a checkpoint pair. The detail is not lost — it is `abcc replay`'s job,
/// and this verb's job is the patch.
fn first_line(why: &str) -> &str {
    why.lines().next().unwrap_or(why).trim_end()
}

/// The patch has to be one this can apply whole, or none of it.
fn refuse_unlandable(patch: &str) -> Result<(), AppError> {
    if patch.trim().is_empty() {
        return Err(AppError::Refused(
            "the gate was green over a tree that changed nothing, so there is no commit to make"
                .to_owned(),
        ));
    }
    // `patch_between` is the Judge's pre-image and carries no `--binary`, so a
    // binary change arrives as a sentence rather than as hunks. Applying the
    // rest would land part of a change.
    if let Some(line) = patch.lines().find(|l| l.starts_with("Binary files ")) {
        return Err(AppError::Refused(format!(
            "this change has a binary hunk and the patch only says so ({line}). Landing the rest \
             would put part of a change on the branch, which is worse than none of it."
        )));
    }
    Ok(())
}

fn sha(raw: &str) -> Result<Sha, AppError> {
    Sha::parse(raw)
        .map_err(|got| AppError::Refused(format!("the log holds a bad checkpoint: {got}")))
}

/// The commit message.
///
/// 🚨 **The `Co-authored-by:` trailer is not decoration** (F433, `CLAUDE.md`).
/// This family squash-merges — 471 commits and zero merge commits in the donor —
/// so after a squash the trailer is *the only surviving record of authorship*,
/// and W13 accepts that cost only because the trailer is machine-readable. A
/// landing that dropped it would make the agent-authored share uncountable at
/// exactly the moment it starts to matter.
fn message(trace: &TaskTrace, landing: &Landing) -> String {
    format!(
        "{}\n\nLanded from {} by `abcc land`: {} rung(s) green at {}.\n\
         Checkpoints {}..{}.\n\n\
         Co-authored-by: abcc <abcc@localhost>\n\
         Landed-by: {}\n",
        trace.title,
        landing.attempt,
        landing.rungs,
        &landing.to[..7],
        &landing.from[..7],
        &landing.to[..7],
        operator(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use abcc_core::attempt::Cause;
    use abcc_core::outcome::Measurement;
    use abcc_core::replay::{Made, Spend};
    use abcc_core::seq::{CheckpointId, UnitId};

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn measured(rung: &str, sha: &str, exit: i32) -> Outcome {
        Outcome::Measured(Measurement {
            rung: rung.to_owned(),
            sha: sha.to_owned(),
            exit,
            counts: None,
            detail: String::new(),
        })
    }

    fn trace_with(attempts: Vec<AttemptTrace>) -> TaskTrace {
        TaskTrace {
            id: TaskId::at(Seq::new(1)),
            mission: abcc_core::seq::MissionId::at(Seq::ORIGIN),
            title: "a task".to_owned(),
            prompt: "do the thing".to_owned(),
            created_ms: 0,
            state: None,
            transitions: Vec::new(),
            attempts,
            asked: Vec::new(),
        }
    }

    fn attempt(seq: i64, opened_at: Option<i64>, rungs: Vec<Outcome>) -> AttemptTrace {
        AttemptTrace {
            id: AttemptId::at(Seq::new(seq)),
            unit: UnitId(0),
            cause: Cause::Fresh,
            checkpoint_from: opened_at.map(|s| CheckpointId::at(Seq::new(s))),
            started: Seq::new(seq),
            started_ms: 0,
            ended: None,
            ended_ms: None,
            outcome: None,
            phases: Vec::new(),
            tools: Vec::new(),
            spend: Spend::default(),
            made: Made::default(),
            discarded: Vec::new(),
            nudges: 0,
            marks: 0,
            claims: 0,
            rungs,
            quiet: None,
            over_bar: 0,
            open_phase: None,
            landed: None,
        }
    }

    fn points() -> HashMap<Seq, String> {
        HashMap::from([(Seq::new(5), A.to_owned())])
    }

    #[test]
    fn a_green_attempt_lands_from_the_pair_its_rungs_name() {
        let t = trace_with(vec![attempt(
            10,
            Some(5),
            vec![measured("structural", B, 0), measured("acceptance", B, 0)],
        )]);
        let got = landing(&t, &points()).expect("landable");
        assert_eq!(got.from, A);
        assert_eq!(got.to, B);
        assert_eq!(got.rungs, 2);
    }

    /// 🚨 The entitlement is the whole gate, not a majority of it. One red rung
    /// is `Headline::Red` and `Headline::is_pass` is the only reader of that.
    #[test]
    fn one_red_rung_is_not_entitled_to_land() {
        let t = trace_with(vec![attempt(
            10,
            Some(5),
            vec![measured("structural", B, 0), measured("standard", B, 101)],
        )]);
        let err = landing(&t, &points()).expect_err("refused");
        assert!(err.contains("no attempt"), "{err}");
        assert!(err.contains("standard"), "{err}");
    }

    /// 🚨 ADR-0009 §6, inherited rather than re-implemented: rungs measured at
    /// two different trees have not measured one tree, so the headline is
    /// `Unverified` and this refuses. Without `headline_at` both rungs are green
    /// and this would land a pair neither of them saw whole.
    #[test]
    fn rungs_measured_at_two_shas_are_not_green() {
        let t = trace_with(vec![attempt(
            10,
            Some(5),
            vec![measured("structural", B, 0), measured("acceptance", C, 0)],
        )]);
        let err = landing(&t, &points()).expect_err("refused");
        assert!(err.contains("unverified"), "{err}");
        // The staleness names both trees, which is the whole of ADR-0009 §6:
        // the rung was measured somewhere the landing is not.
        assert!(err.contains("acceptance: measured at"), "{err}");
        assert!(err.contains(C) && err.contains(B), "{err}");
    }

    /// An absence is not a failure and is not a pass either — the one outcome
    /// the donors do not have.
    #[test]
    fn an_unmeasured_rung_is_not_green() {
        let t = trace_with(vec![attempt(
            10,
            Some(5),
            vec![
                measured("structural", B, 0),
                Outcome::Unmeasured {
                    rung: "acceptance".to_owned(),
                    why: abcc_core::outcome::Why::CheckerNotOnHost {
                        binary: "cargo".to_owned(),
                    },
                },
            ],
        )]);
        let err = landing(&t, &points()).expect_err("refused");
        assert!(err.contains("no attempt"), "{err}");
    }

    /// 🚨 Newest first: a retry forks from the earlier attempt's checkpoint, so
    /// landing an older green attempt lands a tree the task has moved past.
    #[test]
    fn the_newest_green_attempt_wins() {
        let t = trace_with(vec![
            attempt(10, Some(5), vec![measured("structural", B, 0)]),
            attempt(20, Some(5), vec![measured("structural", C, 0)]),
        ]);
        let got = landing(&t, &points()).expect("landable");
        assert_eq!(got.attempt, AttemptId::at(Seq::new(20)));
        assert_eq!(got.to, C);
    }

    #[test]
    fn a_task_that_never_ran_says_so_rather_than_reporting_no_green_attempt() {
        let err = landing(&trace_with(Vec::new()), &points()).expect_err("refused");
        assert!(err.contains("never run"), "{err}");
    }

    /// The unit of the ladder is the change, so a second landing would be two
    /// commits of one change and two rows where there is one piece of work.
    #[test]
    fn a_task_already_landed_refuses() {
        let mut a = attempt(10, Some(5), vec![measured("structural", B, 0)]);
        a.landed = Some("deadbeef".to_owned());
        let err = landing(&trace_with(vec![a]), &points()).expect_err("refused");
        assert!(err.contains("already landed"), "{err}");
        assert!(err.contains("deadbeef"), "{err}");
    }

    /// A green gate whose opening checkpoint is not on the log has no pair, and
    /// saying so beats diffing against `HEAD` — which is F329's 241 phantom
    /// files.
    #[test]
    fn a_green_attempt_with_no_opening_checkpoint_refuses() {
        let t = trace_with(vec![attempt(10, None, vec![measured("structural", B, 0)])]);
        let err = landing(&t, &points()).expect_err("refused");
        assert!(err.contains("checkpoint"), "{err}");
    }

    #[test]
    fn an_empty_patch_is_refused_rather_than_committed() {
        let err = refuse_unlandable("   \n").expect_err("refused");
        assert!(format!("{err}").contains("changed nothing"), "{err}");
    }

    /// 🚨 `patch_between` is the Judge's pre-image and carries no `--binary`, so
    /// this line is the whole of a binary change. Applying the rest would land
    /// half of one.
    #[test]
    fn a_binary_hunk_is_refused_rather_than_half_applied() {
        let patch = "diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n";
        let err = refuse_unlandable(patch).expect_err("refused");
        assert!(format!("{err}").contains("binary"), "{err}");
    }

    /// F433: after a squash the trailer is the only surviving record of
    /// authorship, so it is asserted rather than trusted to survive an edit.
    #[test]
    fn the_message_carries_a_machine_readable_authorship_trailer() {
        let t = trace_with(Vec::new());
        let msg = message(
            &t,
            &Landing {
                attempt: AttemptId::at(Seq::new(10)),
                from: A.to_owned(),
                to: B.to_owned(),
                rungs: 4,
            },
        );
        assert!(msg.starts_with("a task\n"), "{msg}");
        assert!(
            msg.contains("\nCo-authored-by: abcc <abcc@localhost>\n"),
            "{msg}"
        );
        assert!(msg.contains("4 rung(s) green"), "{msg}");
    }

    #[test]
    fn checkpoints_are_read_from_the_events_by_their_own_seq() {
        let log = vec![abcc_core::event::Logged {
            seq: Seq::new(7),
            at_ms: 0,
            event: Event::CheckpointTaken {
                task: TaskId::at(Seq::new(1)),
                sha: A.to_owned(),
                git_ref: "refs/abcc/checkpoints/m1/7".to_owned(),
            },
        }];
        assert_eq!(
            checkpoints(&log).get(&Seq::new(7)).map(String::as_str),
            Some(A)
        );
    }

    /// ⚠ **The header is one line and a rung's refusal is not.** `cargo test`
    /// names every binary it ran before it says what failed, so the first
    /// version of `abcc diff` printed `Running tests\cli.rs` in the middle of a
    /// sentence about a checkpoint pair.
    #[test]
    fn a_refusal_carrying_a_tools_output_is_cut_to_its_first_line() {
        let captured = "acceptance failed: error: test failed\n\
                        Running tests\\cli.rs\n\
                        Running tests\\land.rs";
        assert_eq!(
            first_line(captured),
            "acceptance failed: error: test failed"
        );
        assert_eq!(first_line("one line only"), "one line only");
        assert_eq!(first_line(""), "");
    }
}
