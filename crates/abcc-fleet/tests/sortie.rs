//! The fleet against a real repository and a scripted model: **does the retry
//! budget actually get spent, and does it actually stop?**
//!
//! These are the Fleet milestone's exit questions asked of the code rather than
//! of the design. Every one of them was answerable "yes, by construction" from
//! the ADRs before any of it ran, and F548 is what that was worth: the driver
//! recommended a retry and landed the task in a terminal state, and the two had
//! never been read together because nothing had ever received a `NextAction`.
//!
//! ⚠ The model is scripted and the toolchain is not configured, so no rung runs
//! and no `cargo` is spawned. What is under test is the fleet's arithmetic and
//! the state machine underneath it — `abcc-gate` is where the rungs are.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;

use abcc_core::attempt::{AttemptOutcome, Cause, NextAction};
use abcc_core::event::Event;
use abcc_core::seq::{MissionId, Seq, TaskId};
use abcc_core::task::TaskState;
use abcc_engine::Head;
use abcc_engine::control::ControlPoint;
use abcc_engine::scripted::{Script, Scripted};
use abcc_fleet::{Admission, Fleet, Grounded, budget};
use abcc_store::Store;
use abcc_vcs::Repo;

const MODEL: &str = "scripted";
const PROMPT: &str = "make one() return two";

fn git(cwd: &Path, args: &[&str]) {
    let status = OsCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

struct Subject {
    /// Held so the temp tree outlives the test.
    _dir: tempfile::TempDir,
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
        _dir: dir,
        root,
        worktrees,
    }
}

/// One task on the board, queued.
fn seed(store: &mut Store, title: &str) -> TaskId {
    let m = store
        .append(Event::MissionCreated {
            title: "fleet".into(),
        })
        .expect("mission");
    let t = store
        .append(Event::TaskCreated {
            mission: MissionId::at(m.seq),
            title: title.into(),
            prompt: PROMPT.into(),
        })
        .expect("task");
    TaskId::at(t.seq)
}

/// **One attempt's worth of absence**, and it is exactly one script.
///
/// ⚠ Recon hits the cap with an empty payload, which is
/// `Uncertain { TruncatedAtCap }` and retryable — and it means Builders never
/// runs, so an attempt consumes one script and not two. The scripts are one
/// queue across the whole sortie rather than one per attempt, so getting this
/// wrong makes the *next* attempt read the spare and answer.
fn an_absence() -> Script {
    Script::truncated_at_cap(Head::Recon.budget())
}

/// `n` attempts' worth, plus two spares — so that the provider running dry can
/// never be what stopped a sortie.
fn absences(n: usize) -> Vec<Script> {
    (0..n + 2).map(|_| an_absence()).collect()
}

fn causes(store: &Store, task: TaskId) -> Vec<Cause> {
    store
        .task_history(task)
        .expect("history")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::AttemptStarted { cause, .. } => Some(cause),
            _ => None,
        })
        .collect()
}

fn state(store: &Store, task: TaskId) -> TaskState {
    store.task(task).expect("task").expect("row").state
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

/// Admission is a fold over the log, so an empty board admits nothing rather
/// than blocking on an empty queue that does not exist.
#[test]
fn a_quiet_board_admits_nothing() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let repo = Repo::open(&subject.root).expect("open");
    let provider = Scripted::new(vec![]);
    let fleet = Fleet::new(
        &mut store,
        &repo,
        &provider,
        MODEL,
        subject.worktrees.clone(),
    );

    assert_eq!(fleet.admit().expect("admit"), Admission::Quiet);
}

/// The cause is **derived from the log**, never chosen: a task with no attempts
/// is fresh, and there is no third answer.
#[test]
fn a_task_that_has_never_run_is_admitted_fresh() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    let repo = Repo::open(&subject.root).expect("open");
    let provider = Scripted::new(vec![]);
    let fleet = Fleet::new(
        &mut store,
        &repo,
        &provider,
        MODEL,
        subject.worktrees.clone(),
    );

    assert_eq!(
        fleet.admit().expect("admit"),
        Admission::Run {
            task,
            cause: Cause::Fresh
        }
    );
}

// ---------------------------------------------------------------------------
// The budget, spent for real
// ---------------------------------------------------------------------------

/// 🚨🚨 **The Fleet milestone's centre of gravity: two attempts and no more.**
///
/// The model produces an absence every time, which is the retryable ending. The
/// fleet must buy exactly one retry, then stop and ask a person — and it must
/// stop by itself, because the loop's terminating condition is the budget and
/// nothing else.
#[test]
fn a_task_that_keeps_producing_an_absence_gets_two_attempts_and_then_a_person() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    let repo = Repo::open(&subject.root).expect("open");

    let provider = Scripted::new(absences(2));

    let (mut control, _handle) = ControlPoint::new();
    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie(&mut control).expect("sortie")
    };

    assert_eq!(sortie.grounded, Grounded::Quiet);
    assert_eq!(sortie.flown.len(), 2, "the budget bought the wrong number");
    assert_eq!(
        provider.remaining(),
        2,
        "a third attempt started and ate a spare script"
    );

    // The lineage says what the two were, and it cannot be lost: `Cause` is
    // written once at `AttemptStarted` and attempts are immutable.
    let seen = causes(&store, task);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], Cause::Fresh);
    assert!(matches!(seen[1], Cause::Retry { .. }));
    assert_eq!(budget::spent(&seen), budget::ATTEMPTS);

    // The first landed back on the board so the second could start; the second
    // landed in front of a person, because the budget was gone.
    assert_eq!(
        sortie.flown[0].state,
        TaskState::Queued,
        "the first attempt did not leave the task admissible"
    );
    assert!(
        matches!(sortie.flown[1].state, TaskState::AwaitingOrders { .. }),
        "{:?}",
        sortie.flown[1].state
    );
    assert_eq!(state(&store, task), sortie.flown[1].state);

    // ⚠ The recommendation is `Attempt` **both times**. What the ending was is a
    // fact about the attempt; what the fleet could afford is not, and collapsing
    // the two is how F548 happened.
    for landed in &sortie.flown {
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
        assert!(matches!(landed.outcome, AttemptOutcome::Uncertain { .. }));
    }
}

/// The board does not sit spinning on a task it will not admit: a `Queued` task
/// whose budget is gone grounds the sortie and says so on the log.
///
/// ⚠ This state should be unreachable through the landings. It is tested because
/// *unreachable* is a claim about code that changes, and the alternative to
/// noticing is a loop that spends the GPU forever.
#[test]
fn a_queued_task_whose_budget_is_gone_grounds_the_sortie_instead_of_looping() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    let repo = Repo::open(&subject.root).expect("open");

    let provider = Scripted::new(absences(2));
    let (mut control, _handle) = ControlPoint::new();
    {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie(&mut control).expect("sortie");
    }

    // Put it back on the board behind the fleet's back, which is what a landing
    // that got this wrong would do.
    store
        .apply(task, abcc_core::task::Command::Commandeer)
        .expect("commandeer");
    store
        .apply(task, abcc_core::task::Command::Release)
        .expect("release");
    assert_eq!(state(&store, task), TaskState::Queued);

    let provider = Scripted::new(absences(1));
    let (mut control, _handle) = ControlPoint::new();
    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie(&mut control).expect("sortie")
    };

    assert_eq!(sortie.grounded, Grounded::HeldBack { task });
    assert!(sortie.flown.is_empty(), "it ran the task anyway");
    assert_eq!(provider.remaining(), 3, "a model call was made");

    let notes: Vec<String> = store
        .read_from(Seq::ORIGIN, 10_000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::Note { text } => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("Not admitted")),
        "the log does not say why it stopped: {notes:?}"
    );
}

/// Two tasks, one slot: the fleet finishes one line of enquiry before it starts
/// the next, and the second task's budget is its own.
#[test]
fn one_slot_takes_the_tasks_in_turn_and_each_gets_its_own_budget() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let first = seed(&mut store, "one returns two");
    let second = seed(&mut store, "and again");
    let repo = Repo::open(&subject.root).expect("open");

    // Two tasks × two attempts.
    let provider = Scripted::new(absences(4));

    let (mut control, _handle) = ControlPoint::new();
    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie(&mut control).expect("sortie")
    };

    assert_eq!(sortie.grounded, Grounded::Quiet);
    assert_eq!(sortie.flown.len(), 4);
    assert_eq!(causes(&store, first).len(), 2);
    assert_eq!(causes(&store, second).len(), 2);

    // 🚨 One slot means one at a time, and the log is where that is visible:
    // every attempt on the first task precedes every attempt on the second.
    let order: Vec<TaskId> = store
        .read_from(Seq::ORIGIN, 10_000)
        .expect("read")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::AttemptStarted { task, .. } => Some(task),
            _ => None,
        })
        .collect();
    assert_eq!(order, vec![first, first, second, second]);
}

/// 🚨 **The one thing a sortie stops for.** Two tasks are standing by and the
/// operator halts the first attempt: the fleet comes down rather than moving on
/// to the second.
///
/// Two facts say the same thing and either would do — `Landed::next` is `None`
/// exactly when the ending was the operator's, and a `ControlPoint` latches the
/// first stop and keeps it. The sortie reads the first. ⚠ If it read neither, the
/// latch would silently stop *every* remaining attempt, and a fleet that flew on
/// through a halt would be deciding it had not been stopped.
#[test]
fn an_operators_halt_grounds_the_whole_sortie_and_not_just_the_attempt() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let first = seed(&mut store, "one returns two");
    let second = seed(&mut store, "and again");
    let repo = Repo::open(&subject.root).expect("open");

    let provider = Scripted::new(absences(4));
    let (mut control, handle) = ControlPoint::new();
    handle
        .request(abcc_core::event::Control::Halt)
        .expect("request");

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie(&mut control).expect("sortie")
    };

    assert_eq!(sortie.grounded, Grounded::Operator);
    assert_eq!(sortie.flown.len(), 1, "it flew past the halt");
    assert_eq!(
        sortie.flown[0].next, None,
        "the fleet was offered a recommendation about an ending that was not its own"
    );

    // The second task was never touched, and the first is held rather than
    // requeued: the operator's stop keeps the work.
    assert_eq!(causes(&store, second).len(), 0);
    assert!(
        matches!(state(&store, first), TaskState::Holding { .. }),
        "{:?}",
        state(&store, first)
    );
}
