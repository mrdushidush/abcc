//! One attempt, end to end, against a real repository and a real log.
//!
//! The Skeleton milestone's exit criterion is *one real task runs to a durable
//! terminal state* (`PLAN.md` §3), and the half of it that lives here is the
//! shape of the run: what reaches the log, in what order, and where the task
//! ends up. The other half — that a restart reconstructs it — is
//! `abcc-store`'s `boot_reconstructs_identical_status_from_the_log_alone`, and
//! [`restarting_after_an_attempt_reconstructs_the_same_board`] is the same claim
//! made over a board the driver produced rather than one a test hand-wrote.
//!
//! Every test builds its own git repository, because the claims are about what
//! git and `SQLite` actually do rather than about what this crate believes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;

use abcc_core::attempt::{AttemptOutcome, Cause, NextAction};
use abcc_core::event::{Control, Event, Finish};
use abcc_core::outcome::{Headline, Outcome, Reading, Why};
use abcc_core::redact::{MARKER, Secrets};
use abcc_core::seq::{MissionId, Seq, TaskId, UnitId};
use abcc_core::task::{AbortReason, TaskState};
use abcc_drive::{Driver, Landed};
use abcc_engine::control::{ControlHandle, ControlPoint};
use abcc_engine::provider::{Delta, Role};
use abcc_engine::scripted::{Script, Scripted, Seen};
use abcc_engine::turn::PhaseEnded;
use abcc_engine::workspace::Toolchain;
use abcc_engine::{Head, Limits};
use abcc_store::Store;
use abcc_vcs::{Repo, Sha};

const MODEL: &str = "qwen3.6-35b-a3b-mtp@iq3_s";
const PROMPT: &str = "make one() return two";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

fn git(cwd: &Path, args: &[&str]) {
    let out = OsCommand::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with one commit, and a directory to cut worktrees into that is
/// **outside** it.
struct Subject {
    /// Held so the temp tree outlives the test; the durability test also reads
    /// its path to put a real database file in it.
    dir: tempfile::TempDir,
    root: PathBuf,
    worktrees: PathBuf,
}

fn subject() -> Subject {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("subject");
    fs::create_dir_all(root.join("src")).expect("mkdir");

    git(dir.path(), &["init", "-q", "-b", "main", "subject"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "test"]);

    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").expect("write");
    fs::write(root.join(".gitignore"), "target/\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "first"]);

    let worktrees = dir.path().join("worktrees");
    Subject {
        dir,
        root,
        worktrees,
    }
}

/// A mission with one task on it, queued.
fn seed(store: &mut Store) -> TaskId {
    let m = store
        .append(Event::MissionCreated {
            title: "skeleton".into(),
        })
        .expect("mission");
    let t = store
        .append(Event::TaskCreated {
            mission: MissionId::at(m.seq),
            title: "one returns two".into(),
            prompt: PROMPT.into(),
        })
        .expect("task");
    TaskId::at(t.seq)
}

/// Run one attempt with `scripts` and no operator interference.
fn drive(subject: &Subject, store: &mut Store, task: TaskId, scripts: Vec<Script>) -> Landed {
    driven(subject, store, task, scripts, None)
}

/// The same, with an operator-configured toolchain profile so the gate has rungs
/// to run. ⚠ The profiles below need nothing installed: what is under test is the
/// driver's wiring, not cargo. `abcc-gate`'s own tests are where the rungs are.
fn driven(
    subject: &Subject,
    store: &mut Store,
    task: TaskId,
    scripts: Vec<Script>,
    toolchain: Option<Toolchain>,
) -> Landed {
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(scripts);
    let repo = Repo::open(&subject.root).expect("open");
    let mut driver = Driver::new(store, &repo, &provider, MODEL, &subject.worktrees);
    if let Some(toolchain) = toolchain {
        driver = driver.toolchain(toolchain);
    }
    driver
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run")
}

/// The same, handing back every request the provider saw, so a test can assert
/// what a phase was *shown* rather than only what it produced.
fn watched(
    subject: &Subject,
    store: &mut Store,
    task: TaskId,
    scripts: Vec<Script>,
    toolchain: Option<Toolchain>,
) -> (Landed, Vec<Seen>) {
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(scripts);
    let repo = Repo::open(&subject.root).expect("open");
    let mut driver = Driver::new(store, &repo, &provider, MODEL, &subject.worktrees);
    if let Some(toolchain) = toolchain {
        driver = driver.toolchain(toolchain);
    }
    let landed = driver
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");
    (landed, provider.seen())
}

/// What the review script answers: the shape `judge::REVIEW` asks for, with one
/// finding that carries something runnable.
const REVIEWED: &str = r#"{"assessment":"It adds src/new.rs and leaves one() alone.",
  "findings":[{"at":"src/new.rs:1","defect":"two() is never called",
  "call":"cargo test","expected":"a test exercises two()","actual":"nothing does"}]}"#;

/// A profile whose test command passes and whose standard is not declared, so a
/// changed tree reaches `Green` on three rungs.
#[cfg(windows)]
const PASSING: Toolchain = Toolchain {
    name: "scripted",
    witnesses: &[],
    test: &["cmd", "/C", "echo test result: ok. 2 passed; 0 failed;"],
    diagnostics: &["cmd", "/C", "echo checked"],
    reading: Reading::Cargo,
    standard: None,
};

#[cfg(not(windows))]
const PASSING: Toolchain = Toolchain {
    name: "scripted",
    witnesses: &[],
    test: &["sh", "-c", "echo 'test result: ok. 2 passed; 0 failed;'"],
    diagnostics: &["sh", "-c", "echo checked"],
    reading: Reading::Cargo,
    standard: None,
};

/// The same profile with a red suite.
#[cfg(windows)]
const FAILING: Toolchain = Toolchain {
    test: &[
        "cmd",
        "/C",
        "echo test result: FAILED. 1 passed; 1 failed; & exit 101",
    ],
    ..PASSING
};

#[cfg(not(windows))]
const FAILING: Toolchain = Toolchain {
    test: &[
        "sh",
        "-c",
        "echo 'test result: FAILED. 1 passed; 1 failed;'; exit 101",
    ],
    ..PASSING
};

/// The script pair that answers *and* writes, which is what the gate needs to
/// have anything to measure.
fn changing() -> Vec<Script> {
    vec![
        Script::says("src/lib.rs is the place"),
        Script::calls(
            "c1",
            "write_file",
            r#"{"path":"src/new.rs","content":"pub fn two() -> u32 { 2 }\n"}"#,
        ),
        Script::says("wrote src/new.rs"),
    ]
}

fn rungs(store: &Store) -> Vec<Outcome> {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::RungRecorded { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect()
}

/// How each model call in the attempt ended, in order. The witness for *the
/// phase really was cut off* now that the attempt's own outcome no longer says
/// so — [`Event::PhaseEnded`] deliberately carries the accounting and not the
/// `Why`, and `Landed::change` carries the phase's report and not its ending.
fn finishes(store: &Store) -> Vec<Finish> {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::ModelCallEnded { finish, .. } => Some(finish),
            _ => None,
        })
        .collect()
}

fn question(store: &Store) -> String {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find_map(|l| match l.event {
            Event::OperatorPrompted { question, .. } => Some(question),
            _ => None,
        })
        .expect("no question was put to the operator")
}

fn kinds(store: &Store) -> Vec<&'static str> {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .iter()
        .map(|l| l.event.kind())
        .collect()
}

/// Every checkpoint on the log, as `(sha, ref)` in the order it was taken.
fn checkpoints(store: &Store) -> Vec<(String, String)> {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::CheckpointTaken { sha, git_ref, .. } => Some((sha, git_ref)),
            _ => None,
        })
        .collect()
}

fn worktree_paths(store: &Store) -> Vec<String> {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::WorktreeOpened { path, .. } => Some(path),
            _ => None,
        })
        .collect()
}

/// A stop that is already waiting when the attempt starts, so it is latched at
/// the first step boundary and no model call is made.
fn stopped(subject: &Subject, store: &mut Store, task: TaskId, control: Control) -> Landed {
    let (mut point, handle): (ControlPoint, ControlHandle) = ControlPoint::new();
    handle.request(control).expect("request");
    let provider = Scripted::new(vec![Script::says("never reached")]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(store, &repo, &provider, MODEL, &subject.worktrees)
        .run(task, UnitId(0), Cause::Fresh, &mut point)
        .expect("run")
}

// ---------------------------------------------------------------------------
// The shape of a run
// ---------------------------------------------------------------------------

/// The whole sequence, asserted as a sequence. A driver that took the worktree
/// before the checkpoint, or ended the attempt before closing the worktree,
/// would still pass every individual assertion about those events existing.
#[test]
fn one_attempt_runs_localize_then_change_and_the_log_says_so() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = drive(
        &subject,
        &mut store,
        task,
        vec![
            Script::says("src/lib.rs line 1 is where one() is."),
            Script::says("Changed src/lib.rs to return 2."),
        ],
    );

    assert_eq!(
        kinds(&store),
        vec![
            "mission_created",
            "task_created",
            // Deploy: the slot is granted before anything is snapshotted.
            "task_transitioned",
            // The opening snapshot, then the worktree cut at it, then — and only
            // then — an attempt that has something to tombstone.
            "checkpoint_taken",
            "worktree_opened",
            "attempt_started",
            "task_transitioned", // Engage
            "attempt_phase_entered",
            // 🚨 **F708: the brief the model was shown, before the call that
            // showed it.** It sits under its phase and never carries one: the
            // phase is the `attempt_phase_entered` above, and a second copy is a
            // second thing that can disagree.
            "brief_recorded",
            "model_call_started",
            "model_call_ended",
            "claim_recorded",
            // F513: the phase's accounting, written at the phase's single exit.
            // It follows the claim because the claim is what ended the phase.
            "phase_ended",
            "attempt_phase_entered",
            "brief_recorded",
            "model_call_started",
            "model_call_ended",
            "claim_recorded",
            "phase_ended",
            // The closing snapshot is taken while the worktree still exists.
            "checkpoint_taken",
            "rung_recorded",
            // 🚨 The Judge was not asked, and the log says so rather than
            // staying quiet about it. Neither script wrote anything, so the
            // structural rung refused an empty diff and there is no change to
            // review — which is 16 of this project's own 25 logged attempts
            // (F518), and therefore the common case rather than the corner one.
            "note",
            "worktree_closed",
            "attempt_ended",
            "operator_prompted",
            "task_transitioned", // RequestOrders
        ]
    );
    assert!(landed.change.is_some(), "the Change phase did not run");
    assert_eq!(landed.localize.turns, 1);
    assert!(
        landed.judge.is_none(),
        "the Judge answered about a change that does not exist"
    );
}

/// The Change phase is opened with what Recon reported, and it is a **new body
/// under a different head** rather than a continuation of Recon's.
#[test]
fn the_change_phase_is_given_what_recon_found() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let found = "src/lib.rs line 1 is where one() is.";
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says(found), Script::says("done")]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    let seen = provider.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].head_key, "recon");
    assert_eq!(seen[1].head_key, "builders");
    assert_ne!(
        seen[0].head_prefix, seen[1].head_prefix,
        "two phases shared one system prefix"
    );

    assert!(seen[0].messages[0].content.contains(PROMPT));
    assert_eq!(
        seen[1].messages.len(),
        1,
        "Recon's body leaked into Builders"
    );
    assert!(
        seen[1].messages[0].content.contains(found),
        "Builders was not told what Recon found: {}",
        seen[1].messages[0].content
    );
    assert!(
        seen[1].messages[0].content.contains(PROMPT),
        "Builders was not told the task"
    );
}

// ---------------------------------------------------------------------------
// What the driver says now that a rung exists
// ---------------------------------------------------------------------------

/// 🚨 **The model answered, and the tree did not move.** The free rung refuses
/// it before any checker is spawned, and the attempt is `Refused` rather than
/// uncertain — because *nothing changed* is a measurement, not an absence.
///
/// This shape is not hypothetical: of **25 real attempts** on one task in this
/// project's log, **16 closed on a byte-identical tree** while the model said it
/// was done. It is the exact case a harness with no gate calls success.
///
/// ⚠ The task goes to the operator and not to `Failed`, and the recommendation
/// is another attempt. Those do not disagree: the rule is *a task may not go
/// terminal while something is owed to a person*.
#[test]
fn an_answer_over_an_unchanged_tree_is_refused_by_the_free_rung() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = drive(
        &subject,
        &mut store,
        task,
        vec![Script::says("found it"), Script::says("changed it")],
    );

    let kept = landed.kept.clone().expect("the work was not kept");
    assert_eq!(
        landed.outcome,
        AttemptOutcome::Refused {
            rung: "structural".to_owned(),
            detail: "the attempt changed no file the repository tracks".to_owned(),
        },
        "an answer over an unchanged tree was not refused"
    );
    assert!(landed.outcome.is_retryable());
    assert!(matches!(landed.state, TaskState::AwaitingOrders { .. }));
    assert!(matches!(landed.next, Some(NextAction::Attempt { .. })));

    // 🚨 One rung on the log and no more: the ladder stops at a refusal, so a
    // free rung saves the cold build the other three would have cost.
    let recorded = rungs(&store);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].rung(), "structural");

    let asked = question(&store);
    assert!(asked.contains(&kept), "the question: {asked}");
    assert!(asked.contains("structural"), "the question: {asked}");
}

/// A profile in the tree and a change on disk: every declared rung measured,
/// none refused, and **`Accomplished` is said by a measurement**.
///
/// It is the only path to that word in the whole system, and what cannot reach
/// it is anything the model wrote — a `Claim` has no function that turns it into
/// an `Outcome`.
#[test]
fn a_change_that_passes_every_declared_rung_is_accomplished() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = driven(&subject, &mut store, task, changing(), Some(PASSING));

    assert_eq!(landed.outcome, AttemptOutcome::Success);
    assert!(!landed.outcome.is_retryable(), "success was made retryable");
    assert!(matches!(landed.state, TaskState::Accomplished { .. }));
    assert_eq!(landed.next, Some(NextAction::Stop));

    let gate = landed.gate.as_ref().expect("the gate did not run");
    assert!(gate.accepts());
    assert_eq!(gate.headline, Headline::Green { rungs: 3 });
    assert_eq!(rungs(&store).len(), 3, "every rung is on the log");
}

/// A rung refused. The attempt is `Refused`, the task waits for a person, and the
/// question quotes the evidence the host watched rather than the fact that
/// something failed — a refusal an operator cannot read is one nobody can fix.
#[test]
fn a_change_a_rung_refuses_is_refused_and_the_question_carries_the_evidence() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = driven(&subject, &mut store, task, changing(), Some(FAILING));

    let AttemptOutcome::Refused { rung, detail } = &landed.outcome else {
        panic!("a refused change ended {:?}", landed.outcome);
    };
    assert_eq!(rung, "acceptance");
    assert!(detail.contains("FAILED"), "{detail}");
    assert!(matches!(landed.state, TaskState::AwaitingOrders { .. }));

    let asked = question(&store);
    assert!(asked.contains("acceptance"), "{asked}");
    assert!(asked.contains("FAILED"), "{asked}");
}

/// 🚨 The gate ran and could not measure. That is neither a pass nor a failure,
/// and the operator is told **exactly which rung** was missing — the distinction
/// v1 cannot make at all, because its dispatcher is gated on the word `passed`.
#[test]
fn a_change_with_no_checker_is_uncertain_and_names_what_was_missing() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    // No profile, and the fixture carries no witness file.
    let landed = drive(&subject, &mut store, task, changing());

    let kept = landed.kept.clone().expect("the work was not kept");
    assert_eq!(
        landed.outcome,
        AttemptOutcome::Uncertain {
            why: Why::NoCheckerForArtifact {
                artifact: kept.clone()
            }
        },
    );
    assert!(landed.outcome.is_retryable());
    assert!(matches!(landed.state, TaskState::AwaitingOrders { .. }));

    // 🚨 The absence did not stop the ladder — the veto still ran, and it is on
    // the log beside the rung that could not.
    let recorded = rungs(&store);
    assert_eq!(recorded.len(), 3);
    assert!(matches!(recorded[1], Outcome::Unmeasured { .. }));

    let asked = question(&store);
    assert!(asked.contains("acceptance"), "{asked}");
    assert!(asked.contains(&kept), "{asked}");
}

// ---------------------------------------------------------------------------
// F655 — what the gate is asked *about*
// ---------------------------------------------------------------------------

/// The change phase F655 is about: the tool wrote, and then the model was cut
/// off before it could say it was finished. The tree holds work; the ending is
/// an absence.
fn cut_off_mid_change() -> Vec<Script> {
    vec![
        Script::says("src/lib.rs is the place"),
        Script::calls(
            "c1",
            "write_file",
            r#"{"path":"src/new.rs","content":"pub fn two() -> u32 { 2 }\n"}"#,
        ),
        Script::truncated_at_cap(Head::Builders.budget()),
    ]
}

/// The same ending over a tree nobody wrote to.
fn cut_off_having_written_nothing() -> Vec<Script> {
    vec![
        Script::says("src/lib.rs is the place"),
        Script::calls("c1", "read_file", r#"{"path":"src/lib.rs"}"#),
        Script::truncated_at_cap(Head::Builders.budget()),
    ]
}

/// 🚨🚨 **F655: an ending the model did not choose is still measured, because
/// the tree does not care why the model stopped talking.**
///
/// Before this, the gate was asked only under `PhaseEnded::Answered`. DEBUG-P4
/// flew five sorties on one subject and found **six of ten attempts changed the
/// tree while the gate was asked about one** — and two of the five it skipped
/// passed all 544 tests when rebuilt by hand. The console meanwhile printed
/// *the attempt produced no artifact*, which was false every time.
#[test]
fn an_ending_the_model_did_not_choose_is_measured_when_the_tree_changed() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = driven(
        &subject,
        &mut store,
        task,
        cut_off_mid_change(),
        Some(PASSING),
    );

    // The fixture guard. ⚠ It used to be read off `landed.outcome`, and
    // cannot be any more: David's ruling promotes exactly this ending to
    // `Success`, so the outcome no longer witnesses the absence it is a guard
    // against. The last model call's `Finish` is the same fact one level down,
    // where the promotion cannot reach it.
    assert_eq!(
        finishes(&store).last(),
        Some(&Finish::Length {
            content_empty: true
        }),
        "the fixture stopped being the shape this test is about"
    );
    let gate = landed.gate.as_ref().expect("the gate was not asked");
    assert_eq!(gate.headline, Headline::Green { rungs: 3 });
    assert_eq!(rungs(&store).len(), 3, "every rung is on the log");
}

/// 🚨 **And a green ladder underneath it DOES make the attempt
/// `Accomplished`.** The operator's ruling, 2026-09-10: *the model is allowed to
/// accomplish only if the gate tree is green — and updated.*
///
/// This is the boundary F655 deliberately did not cross, and it was not a
/// technical one: the measurement landed on the log and changed no verdict,
/// because *whether a gate-green tree the model never declared finished may be
/// called done* is a question about who is allowed to stop. The operator was the
/// one entitled to answer it and did. The measurement is allowed to stop it —
/// which is rule 1 of this driver read literally, rather than rule 1 plus an
/// unwritten rider that the model must also have said so.
///
/// ⚠ **Both limbs are asserted by the one headline, and the second is not
/// checked here or anywhere in the driver.** `Green` is every declared rung
/// measured with none red, `Rung::Structural` runs first and refuses a tree that
/// changed no tracked file, and a refusal breaks the walk — so *green* already
/// means *updated*. The test immediately below is the proof standing up:
/// the same ending over an unwritten tree stops at the free rung and is
/// **not** promoted.
#[test]
fn a_green_ladder_under_an_unchosen_ending_is_accomplished() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = driven(
        &subject,
        &mut store,
        task,
        cut_off_mid_change(),
        Some(PASSING),
    );

    assert_eq!(
        landed.gate.as_ref().map(|g| &g.headline),
        Some(&Headline::Green { rungs: 3 }),
        "the fixture must be green for this test to be about anything"
    );
    assert_eq!(
        finishes(&store).last(),
        Some(&Finish::Length {
            content_empty: true
        }),
        "the model must NOT have chosen this ending for this test to be about          anything"
    );
    assert_eq!(
        landed.outcome,
        AttemptOutcome::Success,
        "a green ladder did not promote an ending the model never chose"
    );
    assert!(
        matches!(landed.state, TaskState::Accomplished { .. }),
        "{:?}",
        landed.state
    );
    assert_eq!(landed.next, Some(NextAction::Stop));
    // ⚠ Terminal, and nothing is owed: the operator is prompted for a refusal
    // and for an absence, and this is neither.
    assert!(
        !kinds(&store).contains(&"operator_prompted"),
        "a task went terminal with a question outstanding"
    );
}

/// ⚠ **And it costs the free rung and nothing else when nothing was written.**
///
/// This is the objection the widened arm has to answer, and it is answered by
/// `Rung::Structural` rather than by a check in the driver: the structural rung
/// owns the empty diff, a refusal breaks the walk, so the acceptance command
/// never runs. A driver that re-derived *did anything change* for itself would
/// be a second copy of that rule.
#[test]
fn an_unchanged_tree_stops_at_the_free_rung_however_the_phase_ended() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = driven(
        &subject,
        &mut store,
        task,
        cut_off_having_written_nothing(),
        Some(PASSING),
    );

    let recorded = rungs(&store);
    assert_eq!(recorded.len(), 1, "the ladder walked past a refusal");
    assert_eq!(recorded[0].rung(), "structural");
    assert!(
        matches!(&recorded[0], Outcome::Measured(m) if m.exit == 1),
        "{:?}",
        recorded[0]
    );
    // The ending is still the absence it was; the rung did not become one.
    assert_eq!(
        landed.outcome,
        AttemptOutcome::Uncertain {
            why: Why::TruncatedAtCap {
                budget: Head::Builders.budget()
            }
        }
    );
}

/// 🚨🚨 **The Judge IS asked for an ending the model never chose, when the
/// ladder came back green — because that attempt is going terminal.**
///
/// F655 widened the gate and deliberately left the Judge alone: a model call
/// spent on an attempt that had already run out of room buys prose at the price
/// of the thing it was short of. The operator's ruling of 2026-09-10 changed what
/// that attempt *is*. `a8327` — the first attempt on the `--version` subject ever
/// to land — ended `SaidNothing` with four green rungs, was promoted to
/// `Accomplished`, and the log recorded *the Judge was not asked: the model never
/// said the change was finished* about a task nobody would ever be asked about
/// again. Its diff bypassed the file the task named, and the acceptance rung
/// could not notice: it runs the suite, and no test covers a flag nobody had
/// added yet.
///
/// ⚠ **The dossier is the same dossier.** [`abcc_gate::judge`] is shown the task,
/// the diff and the rungs, and deliberately not the author's completion report
/// (F280–F282: the prose measured *subtractive*, 0 of 3). So a silent ending
/// costs the Judge nothing it needed.
#[test]
fn the_judge_is_asked_for_a_green_ending_the_model_did_not_choose() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let mut scripts = cut_off_mid_change();
    scripts.push(Script::says(REVIEWED));
    let (landed, seen) = watched(&subject, &mut store, task, scripts, Some(PASSING));

    assert_eq!(
        landed.gate.as_ref().map(|g| &g.headline),
        Some(&Headline::Green { rungs: 3 }),
        "the fixture must be green for this test to be about anything"
    );
    assert!(landed.judge.is_some(), "the Judge was not asked");
    assert!(
        seen.iter().any(|s| s.head_key == "commandos"),
        "a task went terminal with nothing having read the diff"
    );
    // ⚠ And it still decides nothing: the landing is the measurement's.
    assert_eq!(landed.outcome, AttemptOutcome::Success);
    assert!(
        matches!(landed.state, TaskState::Accomplished { .. }),
        "{:?}",
        landed.state
    );
}

/// ⚠ **And it is still not asked when the ladder did not come back green**,
/// which is the half of F655's argument that survives: that attempt is going
/// back to a person, who is owed the rungs and not a paragraph about a tree that
/// was already refused.
#[test]
fn the_judge_is_not_asked_for_an_ending_the_model_did_not_choose() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (landed, seen) = watched(
        &subject,
        &mut store,
        task,
        cut_off_having_written_nothing(),
        Some(PASSING),
    );

    assert!(landed.gate.is_some(), "the gate was not asked");
    assert!(
        !landed.gate.as_ref().is_some_and(|g| g.headline.is_pass()),
        "the fixture must NOT be green for this test to be about anything"
    );
    assert!(landed.judge.is_none(), "the Judge ran anyway");
    assert!(
        !seen.iter().any(|s| s.head_key == "commandos"),
        "the review head was used on an attempt the model never finished"
    );

    // And the absence is on the log rather than left as a gap beside the rungs.
    let notes: Vec<String> = store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::Note { text } => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("the Judge was not asked") && n.contains("never said")),
        "{notes:?}"
    );
}

/// The operator's stop is still not ours to judge, and after F655 it is the only
/// way to reach an unasked gate at all.
#[test]
fn an_attempt_the_operator_halted_asks_no_rung() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = stopped(&subject, &mut store, task, Control::Halt);

    assert!(landed.kept.is_some(), "Halt threw the work away");
    assert!(landed.gate.is_none(), "the operator's stop was judged");
    assert!(rungs(&store).is_empty(), "a rung ran on a halted attempt");
}

/// The question names the snapshot, so the operator can look at the tree the
/// sentence is about rather than at whatever the worktree holds now.
#[test]
fn the_operators_question_names_the_snapshot_it_is_about() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = drive(&subject, &mut store, task, changing());
    let kept = landed.kept.clone().expect("the work was not kept");
    let question = store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find_map(|l| match l.event {
            Event::OperatorPrompted { question, .. } => Some(question),
            _ => None,
        })
        .expect("no question was put to the operator");
    assert!(question.contains(&kept), "the question: {question}");
}

/// An absence in Localize ends the attempt there. Running Builders on a phase
/// that produced nothing spends a second model call to produce a second nothing.
#[test]
fn a_localize_that_produces_nothing_ends_the_attempt_before_builders_runs() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::truncated_at_cap(Head::Recon.budget()),
        Script::says("never reached"),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    let landed = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    assert_eq!(provider.remaining(), 1, "Builders was called anyway");
    assert!(landed.change.is_none());
    assert_eq!(
        landed.outcome,
        AttemptOutcome::Uncertain {
            // ⚠ The head's budget, NOT the script's: the `Why` is built from
            // `Head::budget()`, so writing the number here twice is two things
            // that can disagree. Raising the budget to 16384 is what found this.
            why: Why::TruncatedAtCap {
                budget: Head::Recon.budget()
            }
        },
        "an empty payload at the cap became something other than an absence"
    );
    // 🚨 F548: not `Failed`. This driver was built with no fleet behind it
    // — `retry_available` defaults to false — so the ending recommends another
    // attempt and hands the task to a person, which is the honest landing for
    // *one attempt was bought and it produced an absence*. With an attempt in
    // hand the same ending lands `Queued`; see
    // `a_recommended_retry_lands_the_task_back_on_the_board_and_the_retry_runs`.
    assert!(
        matches!(landed.state, TaskState::AwaitingOrders { .. }),
        "{:?}",
        landed.state
    );
    assert!(matches!(
        landed.next,
        Some(NextAction::Attempt {
            cause: Cause::Retry { .. }
        })
    ));
    // The tree is still kept: a failed attempt whose work nobody can look at is
    // a failure report with the evidence deleted.
    assert!(landed.kept.is_some());
}

// ---------------------------------------------------------------------------
// The work, and where it survives
// ---------------------------------------------------------------------------

/// The change the tool made is in the closing snapshot, the snapshot is behind a
/// ref, the worktree is gone, and **the operator's checkout never moved**.
#[test]
fn the_work_survives_as_a_checkpoint_and_the_operators_checkout_does_not_move() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = drive(
        &subject,
        &mut store,
        task,
        vec![
            Script::says("src/lib.rs is the place"),
            Script::calls(
                "c1",
                "write_file",
                r#"{"path":"src/new.rs","content":"pub fn two() -> u32 { 2 }\n"}"#,
            ),
            Script::says("wrote src/new.rs"),
        ],
    );

    let taken = checkpoints(&store);
    assert_eq!(taken.len(), 2, "an opening and a closing snapshot");
    let (before, _) = &taken[0];
    let (after, after_ref) = &taken[1];
    assert_eq!(landed.kept.as_deref(), Some(after.as_str()));

    let repo = Repo::open(&subject.root).expect("open");
    let changed = repo
        .changed_between(
            &Sha::parse(before).expect("sha"),
            &Sha::parse(after).expect("sha"),
        )
        .expect("diff");
    let paths: Vec<&str> = changed.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, vec!["src/new.rs"]);

    // The ref is what stops `git gc --prune=now` collecting the snapshot (F330).
    git(&subject.root, &["rev-parse", "--verify", after_ref]);

    // The worktree is removed and its metadata pruned.
    let opened = worktree_paths(&store);
    assert_eq!(opened.len(), 1);
    assert!(
        !Path::new(&opened[0]).exists(),
        "the worktree at {} outlived the attempt",
        opened[0]
    );

    // 🚨 Nothing about the operator's checkout moved: the file the agent wrote is
    // only in the snapshot.
    assert!(!subject.root.join("src/new.rs").exists());
}

// ---------------------------------------------------------------------------
// The operator's verbs
// ---------------------------------------------------------------------------

/// `Halt` keeps the work: the task goes to `Holding` at a checkpoint, and the
/// verb is acknowledged on the log before anything acts on it.
#[test]
fn halting_keeps_the_work_at_a_checkpoint_and_holds_the_task() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = stopped(&subject, &mut store, task, Control::Halt);

    assert!(matches!(landed.state, TaskState::Holding { .. }));
    assert_eq!(
        landed.outcome,
        AttemptOutcome::Uncertain {
            why: Why::Cancelled {
                by: "operator".into()
            }
        }
    );
    assert!(landed.kept.is_some(), "Halt threw the work away");
    // 🚨 No recommendation. What happens after an operator stops something is
    // the operator's, and the fleet does not get a vote.
    assert_eq!(landed.next, None);

    let seen = kinds(&store);
    let applied = seen
        .iter()
        .position(|k| *k == "control_applied")
        .expect("the verb was never acknowledged");
    let ended = seen
        .iter()
        .position(|k| *k == "attempt_ended")
        .expect("the attempt never ended");
    assert!(
        applied < ended,
        "the attempt ended before the verb was read"
    );
}

/// `Kill` keeps nothing, and that is visible on the log as a closing snapshot
/// that was never taken.
#[test]
fn killing_keeps_nothing_and_aborts_the_task() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = stopped(&subject, &mut store, task, Control::Kill);

    assert!(matches!(
        landed.state,
        TaskState::Aborted {
            reason: AbortReason::Operator { .. },
            ..
        }
    ));
    assert_eq!(landed.kept, None);
    assert_eq!(
        checkpoints(&store).len(),
        1,
        "Kill took a closing snapshot anyway"
    );
    assert!(landed.state.is_terminal());
}

/// `Redirect` keeps the work *and* recommends a forked attempt — because
/// attempts are immutable, so changing the question is a fork with a
/// `Cause::Edit` rather than an edit of the attempt in flight.
#[test]
fn a_redirect_holds_the_work_and_recommends_a_forked_attempt() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let landed = stopped(
        &subject,
        &mut store,
        task,
        Control::Redirect {
            prompt: "make one() return three instead".into(),
        },
    );

    assert!(matches!(landed.state, TaskState::Holding { .. }));
    assert_eq!(
        landed.next,
        Some(NextAction::Attempt {
            cause: Cause::Edit { of: landed.attempt }
        })
    );

    // ⚠ `Cause::Edit` names no prompt, and the prompt is not lost by that: it is
    // on the log, in the verb this landing acknowledged.
    let carried = store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find_map(|l| match l.event {
            Event::ControlApplied {
                control: Control::Redirect { prompt },
                ..
            } => Some(prompt),
            _ => None,
        })
        .expect("the redirect's prompt is nowhere");
    assert_eq!(carried, "make one() return three instead");
}

// ---------------------------------------------------------------------------
// Durability
// ---------------------------------------------------------------------------

/// The Skeleton exit criterion over a board the driver produced: restart, and
/// the status is reconstructed from the log alone. `AwaitingOrders` must survive
/// it — boot re-presents the question rather than answering it (F132).
#[test]
fn restarting_after_an_attempt_reconstructs_the_same_board() {
    let subject = subject();
    let path = subject.dir.path().join("abcc.sqlite3");

    let (task, before) = {
        let mut store = Store::open(&path).expect("store");
        let task = seed(&mut store);
        drive(
            &subject,
            &mut store,
            task,
            vec![Script::says("found it"), Script::says("changed it")],
        );
        (task, store.tasks().expect("tasks"))
    };

    let mut reopened = Store::open(&path).expect("reopen");
    let reconciled = reopened.boot().expect("boot");

    assert_eq!(reopened.tasks().expect("tasks"), before);
    assert_eq!(reconciled.requeued, Vec::<TaskId>::new());
    assert_eq!(
        reconciled.prompts_to_represent,
        vec![task],
        "the operator's question was not re-presented"
    );
}

// ---------------------------------------------------------------------------
// A4 Judge — one model call, no tools, and it decides nothing
// ---------------------------------------------------------------------------

/// What the Judge was actually shown, read off the wire rather than off the
/// brief-building code: the diff, the measurements, `Head::Commandos`, no tools,
/// and a schema.
///
/// 🚨 **And not one word either other unit wrote.** Same model, fresh call:
/// 11/12 reading the diff, 5/12 reading the author's completion report, and
/// **0/3** when the report is added alongside the diff — F281, the finding that
/// says the author's prose is not merely unhelpful but *subtractive*.
#[test]
fn the_judge_is_shown_the_diff_and_the_measurements_and_nothing_either_model_said() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let mut scripts = changing();
    scripts.push(Script::says(REVIEWED));
    let (landed, seen) = watched(&subject, &mut store, task, scripts, Some(PASSING));

    assert!(landed.judge.is_some(), "the Judge was not asked");
    let call = seen.last().expect("no requests were made");
    assert_eq!(call.head_key, "commandos");
    assert!(
        call.head_prefix.contains("You have none."),
        "the review head advertises tools"
    );
    assert_eq!(
        call.schema.map(|s| s.name),
        Some("commandos_review"),
        "the one phase with a declared artifact shape sent no schema"
    );

    // A fresh body: one message, and it is the brief.
    assert_eq!(call.messages.len(), 1, "the Judge continued a conversation");
    assert_eq!(call.messages[0].role, Role::User);
    let brief = &call.messages[0].content;

    assert!(brief.contains("src/new.rs"), "the diff is missing: {brief}");
    assert!(
        brief.contains("+pub fn two() -> u32 { 2 }"),
        "the post-image is missing: {brief}"
    );
    assert!(brief.contains("acceptance"), "the measurements are missing");
    assert!(
        brief.contains("2 passed"),
        "the evidence the host watched is missing"
    );

    for prose in ["src/lib.rs is the place", "wrote src/new.rs"] {
        assert!(
            !brief.contains(prose),
            "the Judge was shown what another unit said: {prose}"
        );
    }
}

/// 🚨 **The whole point, asserted twice over.** The review is damning and the
/// attempt is `Accomplished` anyway, because `Green` is a conjunction of
/// measurements and what the model said is a `Claim` — which attaches to a
/// `Report` and to nothing else.
#[test]
fn a_damning_review_does_not_refuse_a_green_tree() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let mut scripts = changing();
    scripts.push(Script::says(REVIEWED));
    let landed = driven(&subject, &mut store, task, scripts, Some(PASSING));

    assert_eq!(landed.outcome, AttemptOutcome::Success);
    assert!(matches!(landed.state, TaskState::Accomplished { .. }));

    let gate = landed.gate.as_ref().expect("the gate did not run");
    assert_eq!(gate.headline, Headline::Green { rungs: 3 });
    assert!(gate.accepts(), "a report moved the conjunction");

    // The claim reached the report, which is where a claim lives.
    let claims = gate.report.claims();
    assert_eq!(claims.len(), 1, "the review did not reach the report");
    assert_eq!(claims[0].by, "Commandos");
    assert!(
        claims[0].text.contains("two() is never called"),
        "{}",
        claims[0].text
    );
    assert!(
        claims[0].text.contains("cargo test"),
        "a finding lost the thing that runs it"
    );

    // And on the log, verbatim, beside the two the working phases wrote.
    let claimed: Vec<String> = store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::ClaimRecorded { claim, .. } => Some(claim.text),
            _ => None,
        })
        .collect();
    assert_eq!(claimed.len(), 3);
    assert!(claimed[2].contains("never called"), "{}", claimed[2]);
}

/// The other half of the same rule, and the one that is easy to get wrong: **a
/// review that fails is not an attempt that failed.** The provider runs out of
/// script on the third call, so the Judge ends `Unmeasured`, and every fact about
/// the attempt is the one the ladder produced.
#[test]
fn a_review_that_never_arrives_changes_nothing_about_the_ending() {
    let subject = subject();

    let mut with_review = Store::in_memory().expect("store");
    let task = seed(&mut with_review);
    let mut scripts = changing();
    scripts.push(Script::says(REVIEWED));
    let reviewed = driven(&subject, &mut with_review, task, scripts, Some(FAILING));

    let mut without = Store::in_memory().expect("store");
    let task = seed(&mut without);
    let silent = driven(&subject, &mut without, task, changing(), Some(FAILING));

    assert_eq!(reviewed.outcome, silent.outcome);
    assert_eq!(
        reviewed.gate.as_ref().map(|g| &g.headline),
        silent.gate.as_ref().map(|g| &g.headline)
    );
    assert_eq!(reviewed.next, silent.next);
    assert!(matches!(silent.state, TaskState::AwaitingOrders { .. }));
    assert!(matches!(reviewed.state, TaskState::AwaitingOrders { .. }));

    // The phase happened and produced nothing, which is a fact worth keeping.
    assert!(
        matches!(silent.judge, Some(PhaseEnded::Unmeasured { .. })),
        "{:?}",
        silent.judge
    );
    assert!(
        silent
            .gate
            .as_ref()
            .expect("gate")
            .report
            .claims()
            .is_empty(),
        "a review that did not happen left a claim"
    );
}

/// ⚠ **The Judge is not asked about a change that does not exist**, and the log
/// says why rather than staying quiet. The structural rung refuses an empty diff
/// on 16 of this project's own 25 logged attempts (F518), so this is the common
/// case — and each one is a model call not made.
#[test]
fn the_judge_is_not_asked_about_an_empty_diff_and_the_log_says_so() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (landed, seen) = watched(
        &subject,
        &mut store,
        task,
        vec![Script::says("found it"), Script::says("nothing to do")],
        Some(PASSING),
    );

    assert!(landed.judge.is_none());
    assert_eq!(seen.len(), 2, "a third call was made about an empty change");
    assert!(
        !seen.iter().any(|s| s.head_key == "commandos"),
        "the review head was used"
    );

    let notes: Vec<String> = store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::Note { text } => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("the Judge was not asked")),
        "{notes:?}"
    );
}

// ---------------------------------------------------------------------------
// F548 — the recommendation and the landing
// ---------------------------------------------------------------------------

/// 🚨 **F548: a retryable ending lands the task where another attempt can start
/// from, and the recommendation it returns is therefore spendable.**
///
/// This test was written the other way round and it passed: the ending returned
/// `Attempt { Retry }` and landed the task in `Failed`, which is terminal, so
/// `TaskState::apply` refused every command before it reached the transition
/// table and `Driver::run`'s opening `Deploy` could not fire. **The driver was
/// acting on its own recommendation by making it unreachable** — the one thing
/// rule 5 says it does not do — and nothing had noticed because until the Fleet
/// milestone nothing had ever tried to receive a `NextAction`.
///
/// The landing is now `Requeue`, and what this asserts is the whole of the fix:
/// the second attempt **runs**, it is a `Retry` of the first, and the two are one
/// task's lineage rather than two tasks.
#[test]
fn a_recommended_retry_lands_the_task_back_on_the_board_and_the_retry_runs() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::truncated_at_cap(Head::Recon.budget()),
        Script::says("never reached"),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    let first = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .retry_available(true)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    let of = match first.next {
        Some(NextAction::Attempt {
            cause: Cause::Retry { of },
        }) => of,
        other => panic!("the ending stopped recommending a retry: {other:?}"),
    };
    assert_eq!(
        first.state,
        TaskState::Queued,
        "a retryable ending left the task somewhere an attempt cannot start from"
    );

    // Now do exactly what a fleet receiving that recommendation does.
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(changing());
    let second = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(task, UnitId(0), Cause::Retry { of }, &mut control)
        .expect("the recommended retry was refused");

    assert_ne!(second.attempt, first.attempt, "the attempt row was reused");

    // The lineage is on the log, by construction: `Cause` is written once at
    // `AttemptStarted` and attempts are immutable (ADR-0004).
    let causes: Vec<Cause> = store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::AttemptStarted { cause, .. } => Some(cause),
            _ => None,
        })
        .collect();
    assert_eq!(causes, vec![Cause::Fresh, Cause::Retry { of }]);
    assert_eq!(
        causes.iter().filter(|c| c.spends_retry_budget()).count(),
        1,
        "the fresh attempt spent budget, or the retry did not"
    );
}

/// 🚨 **The other half of F548: with no attempt in hand, the same ending hands
/// the task to a person, and the question says the budget is what ran out.**
///
/// ADR-0010's rule is `Attempt` twice, then `HandToOperator`, and this is the
/// only place the hand-off can be made: `RequestOrders` is an edge out of
/// `Engaged`, so by the time a fleet has read `Landed::next` the attempt is over
/// and the task can no longer be moved there. That is why the driver is told
/// whether an attempt is in hand rather than being handed the budget — the
/// number 2 lives in one place and this is not it (F392).
///
/// ⚠ `next` is still `Attempt`. The recommendation does not change with the
/// allowance: what the *ending* was is a fact about the attempt, and what the
/// fleet can afford is not.
#[test]
fn a_retryable_ending_with_no_attempt_in_hand_asks_a_person_and_names_the_budget() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::truncated_at_cap(Head::Recon.budget()),
        Script::says("never reached"),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    let landed = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .retry_available(false)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    assert!(
        matches!(landed.state, TaskState::AwaitingOrders { .. }),
        "{:?}",
        landed.state
    );
    assert!(matches!(
        landed.next,
        Some(NextAction::Attempt {
            cause: Cause::Retry { .. }
        })
    ));

    let asked = question(&store);
    assert!(
        asked.contains("1 attempts") || asked.contains("attempts"),
        "the question does not say how many attempts were bought: {asked}"
    );
    assert!(
        asked.contains("budget"),
        "the question does not say a budget ran out: {asked}"
    );
    // The work is still kept, and the operator is told where.
    assert!(landed.kept.is_some());
}

// ---------------------------------------------------------------------------
// F646 — a held task's way back onto a slot
// ---------------------------------------------------------------------------

/// Walk a task to `Holding` the way an operator's `pause` walks it.
fn hold(store: &mut Store, task: TaskId) {
    store
        .apply(task, abcc_core::task::Command::Deploy { unit: UnitId(0) })
        .expect("deploy");
    let started = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: Cause::Fresh,
            checkpoint_from: None,
        })
        .expect("started");
    store
        .apply(
            task,
            abcc_core::task::Command::Engage {
                attempt: abcc_core::seq::AttemptId::at(started.seq),
            },
        )
        .expect("engage");
    store
        .apply(
            task,
            abcc_core::task::Command::Hold {
                checkpoint: abcc_core::seq::CheckpointId::at(started.seq),
            },
        )
        .expect("hold");
}

/// 🚨🚨 **F646: the way onto a slot depends on where the task was.**
///
/// `Deploy` is `Queued -> Deployed` and `Resume` is `Holding -> Deployed`. They
/// land on the same state, and the driver has always sent the first
/// unconditionally — so a held task could not be run at all, which is the whole
/// of why `Command::Resume` had no caller in the binary.
///
/// ⚠ The assertion is that the attempt **ran**, not that a particular command
/// went out: `TaskState::apply` is the authority on legality (F629), so sending
/// the wrong one here is a `Refused` and the run fails. That makes this a test of
/// the contract rather than of a `match` arm's spelling.
#[test]
fn a_held_task_reaches_the_slot_and_a_queued_one_still_does() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");

    let queued = seed(&mut store);
    let landed = drive(&subject, &mut store, queued, changing());
    assert!(
        !matches!(landed.outcome, AttemptOutcome::HardFailure { .. }),
        "a queued task stopped reaching the slot: {landed:?}"
    );

    let held = seed(&mut store);
    hold(&mut store, held);
    assert!(
        matches!(
            store.task(held).expect("row").expect("row").state,
            TaskState::Holding { .. }
        ),
        "the fixture did not hold the task"
    );

    let landed = drive(&subject, &mut store, held, changing());
    assert!(
        !matches!(landed.outcome, AttemptOutcome::HardFailure { .. }),
        "a held task could not be resumed onto a slot: {landed:?}"
    );
    assert!(
        store
            .task_history(held)
            .expect("history")
            .iter()
            .any(|l| matches!(l.event, Event::AttemptStarted { .. })),
        "the resumed task never started an attempt"
    );
}

/// 🚨 **The redirect's prompt reaches both briefs, and the task's own prompt
/// survives beside it.**
///
/// `Cause::Edit` records *that* the operator changed the question and names the
/// attempt it forked from; it does not carry the words. So the addendum is the
/// only path the prompt has, and it is an addendum on purpose — a brief that
/// replaced the task with the redirect would leave the model holding one sentence
/// with nothing behind it.
#[test]
fn a_redirects_prompt_is_an_addendum_to_both_briefs_and_not_a_replacement() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);
    hold(&mut store, task);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(changing());
    let repo = Repo::open(&subject.root).expect("open");
    let landed = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .redirect(Some("look in src/other.rs instead".to_owned()))
        .run(
            task,
            UnitId(0),
            Cause::Edit {
                of: abcc_core::seq::AttemptId::at(Seq::new(1)),
            },
            &mut control,
        )
        .expect("run");
    assert!(
        !matches!(landed.outcome, AttemptOutcome::HardFailure { .. }),
        "{landed:?}"
    );

    // ⚠ Both *working* heads, and not every call: `Engineering` judges the diff
    // from a fresh body of its own (`judge::brief`), which is a different
    // question and has no business being told what the operator typed at the
    // console. Asserting over every call would have quietly required that.
    let working: Vec<_> = provider
        .seen()
        .into_iter()
        .filter(|call| matches!(call.head_key, "recon" | "builders"))
        .collect();
    assert!(
        working.iter().any(|c| c.head_key == "recon")
            && working.iter().any(|c| c.head_key == "builders"),
        "both working phases should have run: {:?}",
        working.iter().map(|c| c.head_key).collect::<Vec<_>>()
    );
    for call in &working {
        let opening = &call.messages.first().expect("an opening message").content;
        assert!(
            opening.contains("look in src/other.rs instead"),
            "{} lost the redirect: {opening}",
            call.head_key
        );
        assert!(
            opening.contains(PROMPT),
            "{} lost the task's own prompt: {opening}",
            call.head_key
        );
    }
}

/// ⚠ **And an attempt with no redirect behind it says nothing about one.** The
/// addendum is an empty string, so the brief has one shape — a heading that
/// appeared on every ordinary attempt would be teaching the model to look for an
/// instruction that is not there.
#[test]
fn an_ordinary_attempt_carries_no_redirect_heading() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(changing());
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    for call in provider
        .seen()
        .into_iter()
        .filter(|call| matches!(call.head_key, "recon" | "builders"))
    {
        let opening = &call.messages.first().expect("an opening message").content;
        assert!(
            !opening.contains("redirected"),
            "{} grew a redirect heading with no redirect: {opening}",
            call.head_key
        );
        assert!(
            opening.contains(PROMPT),
            "{} lost the prompt",
            call.head_key
        );
    }
}

// ---------------------------------------------------------------------------
// F701/F702 — where an attempt opens, and the brief that promises it
// ---------------------------------------------------------------------------

/// The tree each attempt cut its worktree on, in order.
///
/// 🚨 **This is the witness F701 was proved with on the live log, and the one
/// this crate did not have.** Five sorties produced seven attempts whose seven
/// *distinct* opening checkpoints all named one tree — because every one of them
/// was a fresh snapshot of the same untouched checkout. Asserting over the
/// checkpoint ids would have shown seven different values and hidden it; the sha
/// is the fact.
fn opened_on(store: &Store) -> Vec<String> {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::WorktreeOpened { sha, .. } => Some(sha),
            _ => None,
        })
        .collect()
}

/// The **tree** a checkpoint commit points at.
///
/// 🚨 **Two snapshots of one unchanged checkout are two different commits.**
/// `abcc_drive::snapshot` puts the log head in the commit message so the ref name
/// says where in the replay it was taken, so the commit sha moves even when
/// nothing in the working tree has. The live-log evidence for F701 was a *tree*
/// hash for exactly this reason — seven distinct checkpoints naming one tree
/// `9088b222` — and a test that compared commits would have agreed the seven were
/// seven different things.
fn tree_of(root: &Path, sha: &str) -> String {
    let out = OsCommand::new("git")
        .current_dir(root)
        .args(["rev-parse", &format!("{sha}^{{tree}}")])
        .output()
        .expect("git rev-parse");
    assert!(out.status.success(), "{sha} is not a commit");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// `git show <sha>:<path>`, or `None` when that tree has no such file. Asks git
/// rather than the log, because *the work is in the tree* is a claim about git.
fn show(root: &Path, sha: &str, path: &str) -> Option<String> {
    let out = OsCommand::new("git")
        .current_dir(root)
        .args(["show", &format!("{sha}:{path}")])
        .output()
        .expect("git show");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// An attempt that writes a file and is then cut off mid-phase, so it lands
/// `Queued` with its work at a checkpoint — which is the state a fleet dispatches
/// a retry from (F548).
fn wrote_then_ran_out() -> Vec<Script> {
    vec![
        Script::says("src/lib.rs is the place"),
        Script::calls(
            "c1",
            "write_file",
            r#"{"path":"src/new.rs","content":"pub fn two() -> u32 { 2 }\n"}"#,
        ),
        Script::truncated_at_cap(Head::Builders.budget()),
    ]
}

/// Run a first attempt that leaves work behind, and hand back where it landed.
fn left_work_behind(subject: &Subject, store: &mut Store, task: TaskId) -> Landed {
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(wrote_then_ran_out());
    let repo = Repo::open(&subject.root).expect("open");
    let landed = Driver::new(store, &repo, &provider, MODEL, &subject.worktrees)
        .retry_available(true)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");
    assert_eq!(
        landed.state,
        TaskState::Queued,
        "the setup stopped producing a task an attempt can start from"
    );
    assert!(
        landed.kept.is_some(),
        "the setup stopped keeping the work it is about"
    );
    landed
}

/// The second attempt, whatever its cause.
fn again(subject: &Subject, store: &mut Store, task: TaskId, cause: Cause) -> Landed {
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(store, &repo, &provider, MODEL, &subject.worktrees)
        .run(task, UnitId(0), cause, &mut control)
        .expect("run")
}

/// 🚨 **F701: a retry opens on the tree the attempt it retries left behind, and
/// until 2026-09-11 it opened on the operator's checkout instead.**
///
/// `Driver::open_workspace` took `self.checkpoint(row, "before")` unconditionally
/// and never looked at `Cause`, so every retry in this project's history threw
/// its parent's work away and started from the same untouched tree. Seven places
/// in three crates said otherwise and all seven agreed with each other, which is
/// why nothing caught it — there was no witness outside the prose. This is it.
#[test]
fn a_retry_opens_on_the_tree_the_attempt_it_retries_left_behind() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let first = left_work_behind(&subject, &mut store, task);
    let closing = first.kept.clone().expect("kept");

    let second = again(
        &subject,
        &mut store,
        task,
        Cause::Retry { of: first.attempt },
    );
    assert_ne!(second.attempt, first.attempt, "the attempt row was reused");

    let opened = opened_on(&store);
    assert_eq!(opened.len(), 2, "one worktree per attempt: {opened:?}");
    assert_eq!(
        opened[1], closing,
        "the retry opened on a tree the attempt it retries never produced"
    );
    assert_ne!(
        opened[0], opened[1],
        "both attempts opened on one tree — this is F701 itself, back again"
    );

    // ...and *the work is there* is a claim about git, so ask git.
    assert!(
        show(&subject.root, &opened[1], "src/new.rs").is_some(),
        "the retry's tree does not carry what the first attempt wrote"
    );
    assert!(
        show(&subject.root, &opened[0], "src/new.rs").is_none(),
        "the operator's checkout already had the file, so this proves nothing"
    );
}

/// ⚠ **A fresh attempt still opens on the operator's checkout**, which is the
/// half that must not move: `abcc run` dispatches `Cause::Fresh` for every
/// attempt it makes, including on a task with a history behind it.
#[test]
fn a_fresh_attempt_opens_on_the_operators_checkout_however_much_history_it_has() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let first = left_work_behind(&subject, &mut store, task);
    let _ = again(&subject, &mut store, task, Cause::Fresh);

    // ⚠ The **trees**, not the commits: see [`tree_of`].
    let opened = opened_on(&store);
    assert_eq!(
        tree_of(&subject.root, &opened[0]),
        tree_of(&subject.root, &opened[1]),
        "a fresh attempt inherited something, and `abcc run` only ever says fresh"
    );
    assert_ne!(
        opened[0], opened[1],
        "two snapshots of one checkout became one commit, so this test proves nothing"
    );
    assert!(
        show(&subject.root, &opened[1], "src/new.rs").is_none(),
        "a fresh attempt opened on work it did not do"
    );
    assert!(first.kept.is_some());
}

/// 🚨 **F702: the sentence in the brief and the tree it is about, asserted in one
/// place.**
///
/// `brief::redirected` ends *the tree you are looking at is the one that attempt
/// left behind, at its checkpoint*. That was **false against
/// `Driver::open_workspace`** for the whole of Skeleton, Gate and Fleet, and it
/// was in the model's context on every redirect. The function's own doc comment
/// made the same claim, which is exactly why nothing caught it: **a comment and a
/// prompt that agree with each other are one witness, not two.**
///
/// So this test deliberately asserts *both halves together* — the words the model
/// is shown, and the tree it was actually given. Either one alone is the
/// arrangement that failed.
#[test]
fn a_redirected_attempt_opens_on_the_tree_its_brief_promises() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let first = left_work_behind(&subject, &mut store, task);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .redirect(Some("look in src/other.rs instead".to_owned()))
        .run(
            task,
            UnitId(0),
            Cause::Edit { of: first.attempt },
            &mut control,
        )
        .expect("run");

    // Half one: what the model was told.
    let brief = provider
        .seen()
        .into_iter()
        .find(|call| call.head_key == "recon")
        .and_then(|call| call.messages.first().map(|m| m.content.clone()))
        .expect("Recon was never asked anything");
    assert!(
        brief.contains("the one that attempt left behind"),
        "the promise this test exists to hold the code to is gone: {brief}"
    );

    // Half two: the tree it was given.
    let opened = opened_on(&store);
    assert_eq!(
        opened[1],
        first.kept.expect("kept"),
        "the brief promises the stopped attempt's tree and the driver handed over another"
    );
    assert!(
        show(&subject.root, &opened[1], "src/new.rs").is_some(),
        "the brief says the work it already did is there, and it is not"
    );
}

/// ⚠ **A replay opens where the attempt it replays *started*, not where it
/// stopped.** It is the one parented cause that does not continue work —
/// `Cause::Replay` is the console's after-action re-run and produces none — and a
/// replay from the finished tree replays nothing.
#[test]
fn a_replay_opens_where_the_attempt_it_replays_opened_and_not_where_it_stopped() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let first = left_work_behind(&subject, &mut store, task);
    let _ = again(
        &subject,
        &mut store,
        task,
        Cause::Replay { of: first.attempt },
    );

    let opened = opened_on(&store);
    assert_eq!(
        opened[0], opened[1],
        "a replay started from the tree the attempt finished on, which replays nothing"
    );
    assert!(
        show(&subject.root, &opened[1], "src/new.rs").is_none(),
        "a replay opened on work the attempt it replays had not done yet"
    );
}

/// 🚨 **A retry that inherits a tree and adds nothing to it is refused, and this
/// is the reason the opening checkpoint is the parent's own rather than a fresh
/// snapshot of it.**
///
/// Inheritance moves what the gate's diff is *about*: it is now what this attempt
/// changed, not what the chain changed. That is not a side effect to be tolerated
/// — it is the control. Take it away and a retry could open on a green tree its
/// parent produced, do nothing whatsoever, and reach `Green` through
/// [`abcc_gate::Rung::Structural`] — whose entire job is refusing an unchanged
/// tree — because the tree differs from the operator's checkout. Since the
/// operator's ruling of 2026-09-10 a `Green` ladder is promoted to
/// `Accomplished`, so that is a terminal state reached by an attempt that did no
/// work.
///
/// ⚠ The first attempt here is the one that changed something, and it is still
/// `Queued` when the retry starts — so nothing about *this* refusal is a claim
/// that the work is bad.
#[test]
fn a_retry_that_adds_nothing_to_the_tree_it_inherited_is_refused_by_the_free_rung() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let first = left_work_behind(&subject, &mut store, task);

    // Answers, calls no tool, writes nothing — over a tree that already carries
    // `src/new.rs` because its parent put it there.
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::says("src/new.rs is the place"),
        Script::says("it is already done"),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    let second = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .toolchain(PASSING)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    assert!(
        show(&subject.root, &opened_on(&store)[1], "src/new.rs").is_some(),
        "the retry did not inherit, so this asserts nothing about inheriting"
    );
    assert!(
        matches!(
            second.gate.as_ref().map(|g| &g.headline),
            Some(Headline::Red { .. })
        ),
        "an attempt that added nothing to the tree it was handed was not refused: {:?}",
        second.gate.map(|g| g.headline)
    );
    assert!(
        !matches!(second.state, TaskState::Accomplished { .. }),
        "a retry reached a terminal success having done no work"
    );
}

// ---------------------------------------------------------------------------
// F700 — a gate refusal that reaches the next model and not only the operator
// ---------------------------------------------------------------------------

/// An attempt refused by a deterministic rung, and the task walked back onto the
/// board the way the operator's own verbs walk it: `abcc take` then
/// `abcc release`.
///
/// ⚠ **That detour is the finding underneath the test.** A refusal lands
/// `AwaitingOrders`, and **there is no way back to a slot from there**: the one
/// the table used to declare, `Command::OrdersGiven`, had no sender (F703) and
/// no transition on the archive ever took it, so it was removed (F732). The
/// retry this very ending *recommends* is reachable only by taking the task
/// over and handing it back.
fn refused_then_back_on_the_board(
    subject: &Subject,
    store: &mut Store,
    task: TaskId,
) -> (Landed, String, String) {
    let landed = driven(subject, store, task, changing(), Some(FAILING));
    let (rung, detail) = match &landed.outcome {
        AttemptOutcome::Refused { rung, detail } => (rung.clone(), detail.clone()),
        other => panic!("the setup stopped producing a refusal: {other:?}"),
    };
    assert!(
        matches!(landed.state, TaskState::AwaitingOrders { .. }),
        "a refusal stopped going to a person: {:?}",
        landed.state
    );
    store
        .apply(task, abcc_core::task::Command::Commandeer)
        .expect("take");
    store
        .apply(task, abcc_core::task::Command::Release)
        .expect("release");
    (landed, rung, detail)
}

/// Both working heads' opening bodies, in the order they were asked.
fn briefs(provider: &Scripted) -> Vec<(String, String)> {
    provider
        .seen()
        .into_iter()
        .filter(|call| matches!(call.head_key, "recon" | "builders"))
        .map(|call| {
            let opening = call
                .messages
                .first()
                .expect("an opening message")
                .content
                .clone();
            (call.head_key.to_owned(), opening)
        })
        .collect()
}

/// 🚨 **F700: a gate refusal reached a person and never a model.**
///
/// `brief::refused` writes the rung and the check's own output into
/// `Event::OperatorPrompted`, and the only readers of that event in the whole
/// workspace are `abcc-tui`, `replay` and `fun` — none of which builds a model
/// body. So the retry the same ending *recommends* opened with a brief
/// byte-identical to the fresh attempt's: told the task, and not told that a
/// deterministic check had already refused this exact tree, or which one, or what
/// it said.
///
/// ⚠ **It could not have been fixed before F701.** A brief that says *a check
/// refused the tree you are looking at* is only true if the retry is looking at
/// that tree, and until F701 every retry opened on the operator's untouched
/// checkout. The two are one change in two commits.
#[test]
fn a_retry_is_told_which_rung_refused_the_tree_it_inherited() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (first, rung, detail) = refused_then_back_on_the_board(&subject, &mut store, task);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::says("src/new.rs is the place"),
        Script::says("fixed it"),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    let seen = briefs(&provider);
    assert_eq!(seen.len(), 2, "both phases should have run: {seen:?}");
    for (head, brief) in &seen {
        assert!(
            brief.contains(&rung),
            "{head} was not told which rung refused the tree it is standing on: {brief}"
        );
        assert!(
            brief.contains(detail.trim()),
            "{head} was told the rung but not what it said: {brief}"
        );
        // F512's distinction, and it is the operator's wording rather than a
        // second one invented here.
        assert!(
            brief.contains("cannot land"),
            "{head} was told the work is wrong rather than that it cannot land: {brief}"
        );
    }
}

/// ⚠ **An attempt on a tree nothing refused grows no such heading**, which is the
/// half that keeps the addition honest: the paragraph is evidence, and a brief
/// that carried it unconditionally would be a template.
#[test]
fn an_attempt_on_a_tree_nothing_refused_carries_no_refusal_heading() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    // This first attempt ran out of budget. Nothing measured it and nothing
    // refused it.
    let first = left_work_behind(&subject, &mut store, task);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    for (head, brief) in briefs(&provider) {
        assert!(
            !brief.contains("already refused"),
            "{head} was told a check refused a tree nothing refused: {brief}"
        );
    }
}

/// 🚨 **A replay is told nothing about the refusal, and the reason is the tree.**
///
/// `Cause::Replay` opens where the attempt it replays *started*, so the refused
/// work is not under it and *a check has already refused this tree* would be
/// false. `Driver::refusal_under` reads `Opened::continues` rather than the
/// task's last attempt for exactly this case — the two answers differ here, and
/// the easy one is the wrong one.
#[test]
fn a_replay_is_not_told_what_refused_a_tree_it_is_not_standing_on() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (first, rung, _) = refused_then_back_on_the_board(&subject, &mut store, task);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Replay { of: first.attempt },
            &mut control,
        )
        .expect("run");

    // The tree really is the one the refused attempt started on...
    let opened = opened_on(&store);
    assert!(
        show(&subject.root, &opened[1], "src/new.rs").is_none(),
        "the replay inherited the refused work, so this asserts nothing"
    );
    // ...so the brief does not claim a check refused it.
    for (head, brief) in briefs(&provider) {
        assert!(
            !brief.contains(&rung) && !brief.contains("already refused"),
            "{head} was told about a refusal of a tree it is not looking at: {brief}"
        );
    }
}

/// 🚨 **A retry continues the operator's tree, not the attempt's, when the
/// operator has been in it — and it then says nothing about the refusal.**
///
/// `abcc take` then `abcc release` is the only route a refused task has back onto
/// the board: `AwaitingOrders` has no edge to a slot at all since F732 removed
/// the one nothing sent. `hand_back` snapshots the taken-over
/// worktree as `"operator"`, *after* the attempt's `AttemptEnded` — so a
/// span-bounded read of *the parent attempt's closing checkpoint* would step over
/// it and throw a person's own edits away. That is F701's class of loss and a
/// worse one, so `fork_point` takes the task's **latest** checkpoint, which is
/// `abcc take`'s own rule.
///
/// ⚠ **And the attribution narrows with it.** Once the operator has edited the
/// tree, *a check refused the tree you are looking at* (F700) is no longer a
/// thing anyone can say, so `Opened::continues` goes to `None` and the paragraph
/// disappears. The tree and the sentence about it move together or they drift —
/// which is F702.
#[test]
fn a_retry_after_a_takeover_continues_the_operators_tree_and_claims_no_refusal() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (first, rung, _) = refused_then_back_on_the_board(&subject, &mut store, task);

    // `refused_then_back_on_the_board` walked the task through Commandeer and
    // Release with the raw commands. Do what `abcc release` does on top: put a
    // snapshot of the operator's own work on the log, after the attempt ended.
    let row = store.task(task).expect("read").expect("row");
    fs::write(
        subject.root.join("src/by_hand.rs"),
        "pub fn fixed() -> u32 { 2 }\n",
    )
    .expect("write");
    let repo = Repo::open(&subject.root).expect("open");
    let by_hand = abcc_drive::snapshot(&mut store, &repo, &row, "operator").expect("snapshot");

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    let opened = opened_on(&store);
    let latest = opened.last().expect("the retry opened nothing");
    assert_eq!(
        *latest,
        by_hand.sha.to_string(),
        "the retry opened on the attempt's tree and discarded what the operator did by hand"
    );
    assert!(
        show(&subject.root, latest, "src/by_hand.rs").is_some(),
        "the operator's own file is not in the tree the retry was given"
    );

    for (head, brief) in briefs(&provider) {
        assert!(
            !brief.contains(&rung) && !brief.contains("already refused"),
            "{head} was told a check refused a tree the operator has since edited: {brief}"
        );
    }
}

/// 🚨 **The case the first live arm produced, nine times out of nine, and which
/// the first shipped version of F700 could not see.**
///
/// `refusal_under` originally read `AttemptOutcome::Refused` off `AttemptEnded` —
/// the easy read, already in `task_history`. It fired **zero times in nine
/// attempts**. `a9292` and `a9480` each left a tree the veto rung had refused with
/// *the tree does not build*, E0004, the compiler printing the exact missing arm;
/// both *ended* `Uncertain/BudgetExhausted`, because what ran out was the model's
/// rounds. Their retries inherited those broken trees, were told nothing, read a
/// few files and stopped.
///
/// ▶ **`AttemptOutcome` is about the conversation; the ladder is about the tree.**
/// This crate says so one function over, about `ending`'s use of `why`. Keying a
/// sentence about a tree to how a conversation ended was the defect.
#[test]
fn a_retry_is_told_what_a_rung_refused_even_though_its_parent_ran_out_of_budget() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    // Changes the tree, then runs out of completion budget. The gate is asked
    // anyway (F655), the suite is red, and the ATTEMPT ends `Uncertain`.
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(wrote_then_ran_out());
    let repo = Repo::open(&subject.root).expect("open");
    let first = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .toolchain(FAILING)
        .retry_available(true)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    assert!(
        matches!(first.outcome, AttemptOutcome::Uncertain { .. }),
        "the setup stopped producing the case: {:?}",
        first.outcome
    );
    let red = first
        .gate
        .as_ref()
        .expect("the gate was not asked")
        .report
        .outcomes()
        .iter()
        .find(|o| o.is_red() && o.rung() != "structural")
        .cloned()
        .expect("no rung refused the tree, so this asserts nothing");
    assert_eq!(first.state, TaskState::Queued);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    for (head, brief) in briefs(&provider) {
        assert!(
            brief.contains(red.rung()),
            "{head} inherited a tree the {} rung refused and was not told: {brief}",
            red.rung()
        );
    }
}

/// ⚠ **`Rung::Structural`'s refusal is not repeated to the next attempt**, and
/// that is the same distinction the test above turns on, pointed the other way.
///
/// *The attempt changed no file the repository tracks* is a fact about an
/// **attempt**; every other rung runs a checker on the **tree**. Telling a retry
/// that a check refused this tree because its predecessor did nothing would be
/// true of the predecessor and useless to it — and on the first live arm two of
/// the four refusals were exactly that.
#[test]
fn the_structural_rungs_refusal_is_about_an_attempt_and_is_not_passed_on() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    // Answers, writes nothing: structural refuses and the ladder stops there.
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::says("src/lib.rs is the place"),
        Script::truncated_at_cap(Head::Builders.budget()),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    let first = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .toolchain(PASSING)
        .retry_available(true)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");
    assert!(
        first
            .gate
            .as_ref()
            .expect("gate")
            .report
            .outcomes()
            .iter()
            .any(|o| o.is_red() && o.rung() == "structural"),
        "the setup stopped producing a structural refusal"
    );
    assert_eq!(first.state, TaskState::Queued);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    for (head, brief) in briefs(&provider) {
        assert!(
            !brief.contains("already refused"),
            "{head} was told a check refused the tree because its predecessor did \
             nothing, which is a fact about the predecessor: {brief}"
        );
    }
}

/// 🚨 **A take-over that changed nothing still lets the refusal through — and
/// without this, F700 has no reachable path at all.**
///
/// A refusal lands `AwaitingOrders`, which the fleet never admits (F703), so
/// `abcc take` + `abcc release` is the **only** route a refused task has back onto
/// the board. `hand_back` always writes a checkpoint. So the first version of
/// this rule — *`continues` survives only if the latest checkpoint IS the one that
/// attempt closed on* — went `None` on exactly the path that reaches it, because
/// an operator who read the tree and changed nothing still moved the id.
///
/// ▶ **An identity is not content**, for the third time on this feature (F704).
/// The question is about the tree, so `fork_point` asks git.
#[test]
fn a_takeover_that_changed_nothing_still_lets_the_refusal_reach_the_next_attempt() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (first, rung, _) = refused_then_back_on_the_board(&subject, &mut store, task);

    // Exactly what `abcc take` + `abcc release` do when the operator looks and
    // changes nothing: cut a worktree at the task's last checkpoint (`cut`), then
    // snapshot THAT worktree as `operator` (`hand_back`). ⚠ Snapshotting
    // `subject.root` instead would be a different tree entirely — the work lives
    // in the checkpoint and never in the operator's checkout — and would make
    // this test pass or fail for the wrong reason.
    let row = store.task(task).expect("read").expect("row");
    let repo = Repo::open(&subject.root).expect("open");
    let at = Sha::parse(&first.kept.clone().expect("kept")).expect("sha");
    let taken = repo
        .open_worktree(&subject.worktrees.join("take"), &at)
        .expect("take");
    let inner = Repo::open(taken.path()).expect("open the taken tree");
    let handed_back = abcc_drive::snapshot(&mut store, &inner, &row, "operator").expect("snapshot");
    assert!(
        taken.path().join("src/new.rs").exists(),
        "the operator was handed a tree without the work in it"
    );

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("nothing more to find")]);
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    let opened = opened_on(&store);
    assert_eq!(
        *opened.last().expect("opened"),
        handed_back.sha.to_string(),
        "the retry did not open on what the operator handed back"
    );
    for (head, brief) in briefs(&provider) {
        assert!(
            brief.contains(&rung),
            "{head} was not told what refused the tree, because a checkpoint moved \
             while the tree did not: {brief}"
        );
    }
}

// ---------------------------------------------------------------------------
// F708 - the log says what the model was shown
// ---------------------------------------------------------------------------

/// Every brief the log says a phase opened with, in order.
fn briefs_on_the_log(store: &Store) -> Vec<String> {
    store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::BriefRecorded { text, .. } => Some(text.into_string()),
            _ => None,
        })
        .collect()
}

/// The opening message of every phase the provider was really asked - **one per
/// phase and not one per call**, because every round of a phase re-sends the
/// same opening with the turns appended after it.
fn briefs_the_provider_got(seen: &[Seen]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for call in seen {
        let opening = call
            .messages
            .first()
            .expect("a call with no opening message")
            .content
            .clone();
        if out.last() != Some(&opening) {
            out.push(opening);
        }
    }
    out
}

/// 🚨 **F708: until this test the log could not say what the model was
/// shown, and never could.**
///
/// Thirty event kinds and not one carried a prompt body. `ModelCallStarted`
/// records the provider, the model, the head's key, the ceiling and the budget -
/// every fact about the call except the one the call was made of. So *was the
/// model told* was answered by reading a code path and a precondition, for every
/// prompt-surface arm this project has flown: F649's `write_file` sentence,
/// F655's rescue prose, F531's rung view, F700's refusal paragraph. Each of
/// those is a change to what a model is shown whose only witness is a test like
/// the ones above - true of the build the test ran in, and unreadable from the
/// log of the sortie that was actually flown.
///
/// ⚠ **The comparison is against the provider and never against
/// `brief::localize`.** Re-deriving the expected text from the same function the
/// driver calls would be an oracle comparing a thing with itself: it would agree
/// however wrong both were, and what it has to catch is the log describing a
/// prompt other than the one that was sent.
#[test]
fn the_log_carries_the_brief_every_phase_was_shown_byte_for_byte() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let mut scripts = changing();
    scripts.push(Script::says(REVIEWED));
    let (landed, seen) = watched(&subject, &mut store, task, scripts, Some(PASSING));
    assert!(
        landed.judge.is_some(),
        "the fixture must reach all three phases for this test to be about all three"
    );

    let logged = briefs_on_the_log(&store);
    let sent = briefs_the_provider_got(&seen);
    assert_eq!(
        logged.len(),
        3,
        "one brief per phase, and Localize, Change and Judge all ran: {logged:?}"
    );
    assert_eq!(
        logged, sent,
        "the log's record of what the model was shown is not what the model was shown"
    );
}

/// 🚨 **The F700 arm, asked of the log instead of the code.**
///
/// [`a_retry_is_told_which_rung_refused_the_tree_it_inherited`] reads the
/// provider, so it can only ever say *this build would tell it*. This reads the
/// durable log and nothing else, which is what a sortie flown three weeks ago
/// leaves behind - and it is the difference between an arm that observed its own
/// prompt surface and one that asserted it.
#[test]
fn what_a_retry_was_told_about_the_refusal_is_readable_from_the_log_alone() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (first, rung, detail) = refused_then_back_on_the_board(&subject, &mut store, task);
    let before = briefs_on_the_log(&store).len();

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::says("src/new.rs is the place"),
        Script::says("fixed it"),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .run(
            task,
            UnitId(0),
            Cause::Retry { of: first.attempt },
            &mut control,
        )
        .expect("run");

    let retry = briefs_on_the_log(&store).split_off(before);
    assert_eq!(retry.len(), 2, "both phases should have run: {retry:?}");
    for brief in &retry {
        assert!(
            brief.contains(&rung) && brief.contains(detail.trim()),
            "the log cannot say the retry was told what refused its tree: {brief}"
        );
    }

    // ⚠ And the first attempt's own briefs are on the same log saying the
    // opposite, which is what makes the record worth reading: the difference
    // between the two is in it rather than inferred from a commit.
    for brief in &briefs_on_the_log(&store)[..before] {
        assert!(
            !brief.contains("already refused"),
            "the attempt that had nothing under it was told there was: {brief}"
        );
    }
}

/// 🚨 **A brief is text with a sink, so it goes through the boundary**
/// (ADR-0014 §5) - and it is scrubbed **once, before the send**, so the log
/// holds the bytes the model got rather than a cleaned-up account of them.
///
/// ⚠ The paragraph F700 adds is the output of a check that ran over a tree
/// the model itself wrote, which is exactly the class the boundary exists for.
/// An exemption for the strings we wrote is the first of the exemptions.
#[test]
fn a_secret_in_the_task_reaches_neither_the_model_nor_the_log() {
    const KEY: &str = "sk-live-8a41c0de9f2b47";

    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let m = store
        .append(Event::MissionCreated {
            title: "skeleton".into(),
        })
        .expect("mission");
    let t = store
        .append(Event::TaskCreated {
            mission: MissionId::at(m.seq),
            title: "one returns two".into(),
            prompt: format!("make one() return two, using {KEY}"),
        })
        .expect("task");
    let task = TaskId::at(t.seq);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![Script::says("src/lib.rs is the place")]);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .secrets(Secrets::default().with_literal(KEY))
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    for brief in briefs_the_provider_got(&provider.seen()) {
        assert!(
            !brief.contains(KEY) && brief.contains(MARKER),
            "the model was handed the key: {brief}"
        );
    }
    for brief in briefs_on_the_log(&store) {
        assert!(
            !brief.contains(KEY) && brief.contains(MARKER),
            "the log holds the key: {brief}"
        );
    }
    // The class and the count and never the value: a note quoting what it removed
    // would put the key back on the log one field to the left.
    let notes: Vec<String> = store
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::Note { text } => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("redacted:") && !n.contains(KEY)),
        "nothing on the log says a class was removed: {notes:?}"
    );
}

/// 🚨 **F829 at the attempt level: a runaway is an absence, and its purchase is
/// another attempt.**
///
/// `classify` is exhaustive on purpose — its own comment says a thirteenth `Why`
/// should not be able to arrive and be quietly called a soft failure — so the
/// fifteenth needs an answer here rather than in a match arm nobody drove. The
/// answer is the same one `SaidNothing` gets, for the same measured reason: the
/// trace's size varies 9,942–16,564 characters on *identical* input (F246), so a
/// turn that ran away is a sample and not a property of the task.
#[test]
fn a_runaway_reasoning_trace_is_uncertain_and_buys_another_attempt() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(vec![
        Script::raw(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Reasoning("thinking ".repeat(400))),
        ]),
        Script::says("never reached"),
    ]);
    let repo = Repo::open(&subject.root).expect("open");
    let landed = Driver::new(&mut store, &repo, &provider, MODEL, &subject.worktrees)
        .limits(Limits {
            reasoning_ceiling: 1_000,
            ..Limits::default()
        })
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run");

    assert_eq!(
        provider.remaining(),
        1,
        "Builders ran after a dead Localize"
    );
    match landed.outcome {
        AttemptOutcome::Uncertain {
            why: Why::ReasoningRunaway { ceiling, .. },
        } => assert_eq!(ceiling, 1_000),
        other => panic!("a runaway became something other than an absence: {other:?}"),
    }
    assert!(
        matches!(
            landed.next,
            Some(NextAction::Attempt {
                cause: Cause::Retry { .. }
            })
        ),
        "{:?}",
        landed.next
    );
    // The tree is still kept: a failed attempt whose work nobody can look at is
    // a failure report with the evidence deleted.
    assert!(landed.kept.is_some());
}
