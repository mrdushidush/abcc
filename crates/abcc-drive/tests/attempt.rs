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
use abcc_engine::scripted::{Script, Scripted};
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
            "worktree_closed",
            "attempt_ended",
            "operator_prompted",
            "task_transitioned", // RequestOrders
        ]
    );
    assert!(landed.change.is_some(), "the Change phase did not run");
    assert_eq!(landed.localize.turns, 1);
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
    assert!(matches!(landed.state, TaskState::Failed { .. }));
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
