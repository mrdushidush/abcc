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
use abcc_core::outcome::Why;
use abcc_core::seq::{MissionId, Seq, TaskId, UnitId};
use abcc_core::task::{AbortReason, TaskState};
use abcc_drive::{Driver, Landed};
use abcc_engine::control::{ControlHandle, ControlPoint};
use abcc_engine::scripted::{Script, Scripted};
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
    let (mut control, _handle) = ControlPoint::new();
    let provider = Scripted::new(scripts);
    let repo = Repo::open(&subject.root).expect("open");
    Driver::new(store, &repo, &provider, MODEL, &subject.worktrees)
        .run(task, UnitId(0), Cause::Fresh, &mut control)
        .expect("run")
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
            "attempt_phase_entered",
            "model_call_started",
            "model_call_ended",
            "claim_recorded",
            // The closing snapshot is taken while the worktree still exists.
            "checkpoint_taken",
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
// What the driver refuses to say
// ---------------------------------------------------------------------------

/// 🚨 The load-bearing test of this crate. The model answered, the phases ran,
/// the work is on disk — and **nothing measured it**, so the attempt is
/// `Uncertain` and the task is handed to a human. There is no gate at Skeleton;
/// an `Accomplished` here would be a model's claim standing where a measurement
/// belongs, which is the defect ADR-0009 exists to prevent.
#[test]
fn a_working_attempt_ends_uncertain_because_nothing_measured_it() {
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
        AttemptOutcome::Uncertain {
            why: Why::NoCheckerForArtifact {
                artifact: kept.clone()
            }
        },
        "a working attempt was called something other than unmeasured"
    );
    assert!(
        landed.outcome.is_retryable(),
        "an unmeasured attempt was made unretryable"
    );
    assert!(matches!(landed.state, TaskState::AwaitingOrders { .. }));
    assert!(matches!(
        landed.next,
        Some(NextAction::HandToOperator { .. })
    ));

    // The question names the snapshot, so the operator can look at the tree the
    // sentence is about rather than at whatever the worktree holds now.
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
        Script::truncated_at_cap(8192),
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
            why: Why::TruncatedAtCap { budget: 8192 }
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
