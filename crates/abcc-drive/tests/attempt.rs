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
use abcc_core::event::{Control, Event};
use abcc_core::outcome::{Headline, Outcome, Reading, Why};
use abcc_core::seq::{MissionId, Seq, TaskId, UnitId};
use abcc_core::task::{AbortReason, TaskState};
use abcc_drive::{Driver, Landed};
use abcc_engine::Head;
use abcc_engine::control::{ControlHandle, ControlPoint};
use abcc_engine::provider::Role;
use abcc_engine::scripted::{Script, Scripted, Seen};
use abcc_engine::turn::PhaseEnded;
use abcc_engine::workspace::Toolchain;
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
            "model_call_started",
            "model_call_ended",
            "claim_recorded",
            // F513: the phase's accounting, written at the phase's single exit.
            // It follows the claim because the claim is what ended the phase.
            "phase_ended",
            "attempt_phase_entered",
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

    // The fixture guard, read off the outcome because `Landed::change` carries
    // the phase's *report* and not its ending: this test is only about anything
    // if the change phase really did end in an absence.
    assert_eq!(
        landed.outcome,
        AttemptOutcome::Uncertain {
            why: Why::TruncatedAtCap {
                budget: Head::Builders.budget()
            }
        },
        "the fixture stopped being the shape this test is about"
    );
    let gate = landed.gate.as_ref().expect("the gate was not asked");
    assert_eq!(gate.headline, Headline::Green { rungs: 3 });
    assert_eq!(rungs(&store).len(), 3, "every rung is on the log");
}

/// 🚨 **And a green ladder underneath it does not make the attempt
/// `Accomplished`.**
///
/// This is the boundary F655 deliberately did not cross. `ending` reads the gate
/// only under `Answered`, so the measurement lands on the log and changes no
/// verdict — whether a gate-green tree the model never declared finished may be
/// called done is a question about who is allowed to stop, and it is nobody's to
/// answer by widening a `match` arm.
#[test]
fn a_green_ladder_under_an_unchosen_ending_is_still_uncertain() {
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
        landed.outcome,
        AttemptOutcome::Uncertain {
            why: Why::TruncatedAtCap {
                budget: Head::Builders.budget()
            }
        },
        "a measurement promoted an ending the model never chose"
    );
    assert!(
        !matches!(landed.state, TaskState::Accomplished { .. }),
        "{:?}",
        landed.state
    );
    assert_ne!(landed.next, Some(NextAction::Stop));
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

/// ⚠ **The Judge was not widened with the gate**, because it is a model call
/// that decides nothing, and spending one on an attempt that already ran out of
/// room buys prose at the price of the thing it was short of.
#[test]
fn the_judge_is_not_asked_for_an_ending_the_model_did_not_choose() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);

    let (landed, seen) = watched(
        &subject,
        &mut store,
        task,
        cut_off_mid_change(),
        Some(PASSING),
    );

    assert!(landed.gate.is_some(), "the gate was not asked");
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
