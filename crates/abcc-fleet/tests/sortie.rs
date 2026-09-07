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
//!
//! 🚨 The operator's two surfaces are exercised **through `InFlight`**, which is
//! how a console reaches a sortie, rather than by pre-loading a channel. That is
//! not decoration: a sortie makes a fresh `ControlPoint` per attempt precisely so
//! that a verb cannot outlive the attempt it was for, and a test holding one
//! channel across the whole sortie could not tell that design from the one it
//! replaced.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use abcc_core::attempt::{AttemptOutcome, Cause, NextAction};
use abcc_core::event::{Control, Event};
use abcc_core::seq::{AttemptId, CheckpointId, MissionId, Seq, TaskId, UnitId};
use abcc_core::task::{Command, RequeueReason, TaskState};
use abcc_engine::control::{Delivery, InFlight};
use abcc_engine::provider::{
    ApiRequest, Provider, ProviderClass, ProviderError, ProviderId, TurnStream,
};
use abcc_engine::scripted::{Script, Scripted};
use abcc_engine::{Head, Tier};
use abcc_fleet::{Admission, Fleet, Grounded, StandDown, budget};
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
// F646 — the way back out of `Holding`
// ---------------------------------------------------------------------------

/// Walk a task to `Holding` the way the driver walks it, so the fold under test
/// reads a shape this system can actually produce.
///
/// ⚠ `by` is the verb the driver **acknowledged** — the `ControlApplied` written
/// at rule 4, before anything acts on it — and not the operator's request. That
/// is the distinction the fold turns on, so the fixture has to keep it.
///
/// 🚨 And `cause` is a parameter rather than always `Fresh`, because it is the
/// **line of enquiry** this attempt joins: a fixture that quietly appended a
/// `Fresh` would reset `budget::spent` and hand the budget test a task whose
/// budget it had just refilled. It did, and the test caught it.
fn held_at(store: &mut Store, task: TaskId, cause: Cause, by: Option<Control>) -> AttemptId {
    let unit = UnitId(1);
    store.apply(task, Command::Deploy { unit }).expect("deploy");
    let started = store
        .append(Event::AttemptStarted {
            task,
            unit,
            cause,
            checkpoint_from: None,
        })
        .expect("started");
    let attempt = AttemptId::at(started.seq);
    store
        .apply(task, Command::Engage { attempt })
        .expect("engage");
    if let Some(control) = by {
        store
            .append(Event::ControlApplied { task, control })
            .expect("applied");
    }
    store
        .apply(
            task,
            Command::Hold {
                checkpoint: CheckpointId::at(started.seq),
            },
        )
        .expect("hold");
    attempt
}

fn resume_typed(store: &mut Store, task: TaskId) {
    store
        .append(Event::ControlRequested {
            task,
            control: Control::Resume,
        })
        .expect("requested");
}

/// 🚨 **A hold is the operator having stopped this task, so it stays stopped.**
///
/// The negative side of the whole feature, and it is asserted first because
/// every other test here is only interesting against it: if `Holding` were
/// admitted on sight, `pause` would be a hiccup rather than a stop and the tests
/// below would all pass for the wrong reason.
#[test]
fn a_held_task_nobody_asked_for_is_not_admitted() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    held_at(&mut store, task, Cause::Fresh, Some(Control::Pause));
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

/// `resume t42`, typed after the hold. The same question again, so it spends a
/// retry — which is what the budget is for.
#[test]
fn a_resume_typed_after_the_hold_puts_the_task_back_in_the_pool() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    let attempt = held_at(&mut store, task, Cause::Fresh, Some(Control::Pause));
    resume_typed(&mut store, task);
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
        Admission::Resume {
            task,
            cause: Cause::Retry { of: attempt }
        }
    );
}

/// 🚨🚨 **The loop guard, and it is the one with teeth.**
///
/// The request is read and never consumed — there is no acknowledgement row,
/// because marking an event as used would be a mutation of the log. What keeps
/// this from resuming a task for ever is `since`: a `resume` older than the
/// current hold belonged to a hold the task has already left, and a task that
/// holds again gets a newer `since` that the old request falls behind.
///
/// ⚠ So this is not a spelling test. Drop the `logged.seq > since` and the fleet
/// re-admits a task the operator has since stopped a second time, for ever.
#[test]
fn a_resume_from_before_the_hold_belongs_to_a_hold_the_task_has_left() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");

    // Resumed once, ran, and stopped again — the second hold is the live one and
    // nobody has asked for that one.
    let first = held_at(&mut store, task, Cause::Fresh, Some(Control::Pause));
    resume_typed(&mut store, task);
    store
        .apply(task, Command::Resume { unit: UnitId(1) })
        .expect("resume");
    let second = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(1),
            cause: Cause::Retry { of: first },
            checkpoint_from: None,
        })
        .expect("started");
    store
        .apply(
            task,
            Command::Engage {
                attempt: AttemptId::at(second.seq),
            },
        )
        .expect("engage");
    store
        .append(Event::ControlApplied {
            task,
            control: Control::Halt,
        })
        .expect("applied");
    store
        .apply(
            task,
            Command::Hold {
                checkpoint: CheckpointId::at(second.seq),
            },
        )
        .expect("hold");

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
        Admission::Quiet,
        "the first resume was honoured once and must not be honoured again"
    );
}

/// 🚨 **A redirect needs no second verb.** The operator said what to do instead,
/// which is an answer rather than a stop — and the prompt comes back out of the
/// log so the driver can put it in the brief.
#[test]
fn a_hold_caused_by_a_redirect_is_admitted_with_the_prompt() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    let attempt = held_at(
        &mut store,
        task,
        Cause::Fresh,
        Some(Control::Redirect {
            prompt: "look in src/lib.rs instead".to_owned(),
        }),
    );
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
        Admission::Resume {
            task,
            cause: Cause::Edit { of: attempt }
        }
    );
}

/// 🚨 **`Queued` is scanned first, and the order is the design.** `Queued` is the
/// state whose contract *is* eligible for admission; `Holding` is the operator
/// having stopped one. So a redirect issued mid-sortie cannot jump the board —
/// it is picked up on the next turn of the admission loop.
#[test]
fn fresh_work_is_admitted_before_anything_held() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let redirected = seed(&mut store, "the held one");
    held_at(
        &mut store,
        redirected,
        Cause::Fresh,
        Some(Control::Redirect {
            prompt: "instead".to_owned(),
        }),
    );
    let fresh = seed(&mut store, "the fresh one");
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
            task: fresh,
            cause: Cause::Fresh
        }
    );
}

/// 🚨🚨 **A redirect buys a fresh line of enquiry, and a resume does not.**
///
/// ADR-0010's budget is spent on *one question asked repeatedly*, so
/// `Cause::Edit` resets the chain and `Cause::Retry` extends it. The two are
/// asserted against the **same exhausted task**, which is the only way to see
/// that the budget is asked *beside the cause* rather than before it: asked
/// before, both of these come back `HeldBack`.
#[test]
fn a_redirect_resets_the_spent_budget_and_a_resume_does_not() {
    let subject = subject();
    let repo = Repo::open(&subject.root).expect("open");
    let provider = Scripted::new(vec![]);

    for verb in [
        Control::Redirect {
            prompt: "a different question".to_owned(),
        },
        Control::Pause,
    ] {
        let mut store = Store::in_memory().expect("store");
        let task = seed(&mut store, "one returns two");

        // Spend the whole budget first: one fresh attempt and one retry, both on
        // the log, so the line of enquiry is over before the verb lands.
        let first = store
            .append(Event::AttemptStarted {
                task,
                unit: UnitId(1),
                cause: Cause::Fresh,
                checkpoint_from: None,
            })
            .expect("first");

        // 🚨 The held attempt **is** the retry that exhausts the chain. Landing a
        // third `Fresh` here would refill the budget this test exists to find
        // empty — which is what the first version of it did, and the assertion
        // caught it rather than passing on a task that had been quietly reset.
        let redirected = verb != Control::Pause;
        let held = held_at(
            &mut store,
            task,
            Cause::Retry {
                of: AttemptId::at(first.seq),
            },
            Some(verb),
        );
        if !redirected {
            resume_typed(&mut store, task);
        }

        let fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        let admission = fleet.admit().expect("admit");
        if redirected {
            assert_eq!(
                admission,
                Admission::Resume {
                    task,
                    cause: Cause::Edit { of: held }
                },
                "a redirect is a new question, so the budget resets"
            );
        } else {
            assert_eq!(
                admission,
                Admission::HeldBack {
                    task,
                    spent: budget::ATTEMPTS
                },
                "a resume is the same question, and the budget is gone"
            );
        }
    }
}

/// 🚨🚨 **F647: a redirect outlives the attempt that was admitted for it, and
/// the first version of this shipped a prompt that survived exactly one.**
///
/// The words were carried on `Admission::Resume`, which is where the fold that
/// found them happened to be standing. But a redirected attempt that ends in an
/// absence lands `Queued`, and the `Queued` pass has no redirect field — so the
/// retry ran on the **original** prompt, and the log would have shown two
/// attempts inside one `Cause::Edit` line of enquiry that were asked different
/// questions.
///
/// ⚠ The task here is `Queued`, not `Holding`. That is the whole point: nothing
/// about this admission mentions a redirect, and the instruction still applies.
#[test]
fn a_redirect_still_applies_to_the_retry_after_the_attempt_it_redirected() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");

    // Redirected, admitted, flown — and the attempt ended in an absence, which
    // is the landing that puts a task back on the board.
    held_at(
        &mut store,
        task,
        Cause::Fresh,
        Some(Control::Redirect {
            prompt: "look in src/other.rs instead".to_owned(),
        }),
    );
    let edited = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(1),
            cause: Cause::Edit {
                of: AttemptId::at(Seq::new(1)),
            },
            checkpoint_from: None,
        })
        .expect("edited");
    store
        .apply(task, Command::Resume { unit: UnitId(1) })
        .expect("resume");
    store
        .apply(
            task,
            Command::Engage {
                attempt: AttemptId::at(edited.seq),
            },
        )
        .expect("engage");
    store
        .apply(
            task,
            Command::Requeue {
                why: RequeueReason::AttemptRetryable {
                    of: AttemptId::at(edited.seq),
                },
            },
        )
        .expect("requeue");

    let repo = Repo::open(&subject.root).expect("open");
    let provider = Scripted::new(vec![]);
    let fleet = Fleet::new(
        &mut store,
        &repo,
        &provider,
        MODEL,
        subject.worktrees.clone(),
    );

    // The ordinary `Queued` admission — nothing here knows about a redirect.
    assert_eq!(
        fleet.admit().expect("admit"),
        Admission::Run {
            task,
            cause: Cause::Retry {
                of: AttemptId::at(edited.seq)
            }
        }
    );
    // And the instruction is still in force, which is what the driver is handed.
    assert_eq!(
        fleet.redirect_in_force(task).expect("in force"),
        Some("look in src/other.rs instead".to_owned()),
        "the retry would have run on the original prompt"
    );
}

/// ⚠ **And nothing supersedes a redirect but another redirect.** A `pause` after
/// one does not withdraw it: the operator said what to do instead and then
/// stopped the work, which are two statements and not a contradiction.
#[test]
fn a_pause_after_a_redirect_does_not_withdraw_it() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    held_at(
        &mut store,
        task,
        Cause::Fresh,
        Some(Control::Redirect {
            prompt: "the new question".to_owned(),
        }),
    );
    store
        .append(Event::ControlApplied {
            task,
            control: Control::Pause,
        })
        .expect("paused");

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
        fleet.redirect_in_force(task).expect("in force"),
        Some("the new question".to_owned())
    );
    // 🚨 But the *hold* is now a pause's, so it waits for a `resume` rather than
    // picking itself up. The two folds answer different questions on purpose.
    assert_eq!(fleet.admit().expect("admit"), Admission::Quiet);
}

/// A task nobody has ever redirected is working under no instruction, and the
/// fold says so rather than returning the last thing it saw.
#[test]
fn a_task_that_was_never_redirected_is_under_no_instruction() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    held_at(&mut store, task, Cause::Fresh, Some(Control::Halt));

    let repo = Repo::open(&subject.root).expect("open");
    let provider = Scripted::new(vec![]);
    let fleet = Fleet::new(
        &mut store,
        &repo,
        &provider,
        MODEL,
        subject.worktrees.clone(),
    );
    assert_eq!(fleet.redirect_in_force(task).expect("in force"), None);
}

/// ⚠ **The most recent redirect is the one in force.** An operator who redirects
/// twice means the second thing; a fold that took the first would pin the task
/// to an instruction that has been superseded, and it would look right for as
/// long as nobody redirected twice.
#[test]
fn the_most_recent_redirect_is_the_one_in_force() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    for prompt in ["the first thought", "no, this one"] {
        held_at(
            &mut store,
            task,
            Cause::Fresh,
            Some(Control::Redirect {
                prompt: prompt.to_owned(),
            }),
        );
        store
            .apply(task, Command::Resume { unit: UnitId(1) })
            .expect("resume");
        store
            .apply(
                task,
                Command::Requeue {
                    why: RequeueReason::OrphanedByRestart,
                },
            )
            .expect("requeue");
    }

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
        fleet.redirect_in_force(task).expect("in force"),
        Some("no, this one".to_owned()),
        "a superseded instruction is still in force"
    );
}

/// 🚨🚨 **The wire between the fold and the attempt, asserted at the fleet level
/// — because F646 is a whole finding about a fold nothing consumed.**
///
/// `abcc-drive`'s own tests prove the driver puts a redirect it is *given* into
/// both briefs. Nothing proved the sortie gives it one. That is the same
/// one-sided wiring this session exists to fix, one layer up, and a mutation that
/// replaced `.redirect(redirect)` with `.redirect(None)` passed every other test
/// in this file.
#[test]
fn a_sortie_hands_the_redirect_in_force_to_the_attempt_it_flies() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store, "one returns two");
    held_at(
        &mut store,
        task,
        Cause::Fresh,
        Some(Control::Redirect {
            prompt: "look in src/other.rs instead".to_owned(),
        }),
    );
    let repo = Repo::open(&subject.root).expect("open");
    let provider = Scripted::new(absences(1));

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie().expect("sortie")
    };
    assert!(
        !sortie.flown.is_empty(),
        "the held task was never admitted, so nothing was shown a brief"
    );

    let seen = provider.seen();
    let opening = &seen
        .first()
        .expect("the attempt made at least one model call")
        .messages
        .first()
        .expect("an opening message")
        .content;
    assert!(
        opening.contains("look in src/other.rs instead"),
        "the sortie flew the attempt without the instruction that admitted it: {opening}"
    );
    assert!(
        opening.contains(PROMPT),
        "the task's own prompt was replaced rather than added to: {opening}"
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

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie().expect("sortie")
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
    {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie().expect("sortie");
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
    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie().expect("sortie")
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

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        fleet.sortie().expect("sortie")
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
///
/// ⚠ The halt is delivered the way a console delivers one — named, through
/// `InFlight`, while the attempt is running — because that path and the name
/// check in it are the thing being trusted.
#[test]
fn an_operators_halt_grounds_the_whole_sortie_and_not_just_the_attempt() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let first = seed(&mut store, "one returns two");
    let second = seed(&mut store, "and again");
    let repo = Repo::open(&subject.root).expect("open");

    let in_flight = InFlight::default();
    let provider = OnFirstCall::new(absences(4), {
        let in_flight = in_flight.clone();
        move || {
            assert_eq!(
                in_flight.deliver(first, abcc_core::event::Control::Halt),
                Delivery::Sent,
                "the slot was not on the task the operator named"
            );
        }
    });

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        )
        .in_flight(in_flight.clone());
        fleet.sortie().expect("sortie")
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

    // ⚠ And the slot is empty afterwards. A stale entry would take a later verb
    // and report it as delivered to a worker that is not there.
    assert_eq!(in_flight.flying(), None);
}

// ---------------------------------------------------------------------------
// The operator's two fleet-level surfaces
// ---------------------------------------------------------------------------

/// 🚨 **`ground`: fly out what is in flight, then admit nothing more.**
///
/// The flag is raised during the first attempt's first model call, which is the
/// only place a synchronous test can stand in for an operator typing while
/// something runs. The attempt therefore lands *normally* — the assertions below
/// are that it did, and that the second task was never admitted.
///
/// ⚠ This is the stop that did not exist. Before it, ending a sortie early meant
/// killing an attempt nobody objected to, or `Ctrl-C`.
#[test]
fn a_stand_down_lets_the_attempt_in_flight_land_and_admits_nothing_after_it() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let first = seed(&mut store, "one returns two");
    let second = seed(&mut store, "and again");
    let repo = Repo::open(&subject.root).expect("open");

    let stand_down = StandDown::default();
    let provider = OnFirstCall::new(absences(4), {
        let stand_down = stand_down.clone();
        move || stand_down.order()
    });

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        )
        .stand_down(stand_down);
        fleet.sortie().expect("sortie")
    };

    assert_eq!(sortie.grounded, Grounded::StoodDown);
    assert_eq!(
        sortie.flown.len(),
        1,
        "it admitted something after the order"
    );

    // 🚨 The attempt landed on its own terms. A stand-down that reached into the
    // attempt would show here as `next: None` and a `Holding` task, which is what
    // `halt` does and what this deliberately does not.
    assert!(
        sortie.flown[0].next.is_some(),
        "the stand-down ended the attempt instead of letting it land"
    );
    assert_eq!(
        state(&store, first),
        TaskState::Queued,
        "the task did not land back on the board"
    );
    assert_eq!(causes(&store, first).len(), 1);
    assert_eq!(
        causes(&store, second).len(),
        0,
        "the second task was admitted"
    );
}

/// A sortie stood down before it starts flies nothing at all, and says so.
#[test]
fn a_sortie_stood_down_before_it_starts_admits_nothing() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    seed(&mut store, "one returns two");
    let repo = Repo::open(&subject.root).expect("open");

    let provider = Scripted::new(absences(2));
    let stand_down = StandDown::default();
    stand_down.order();

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        )
        .stand_down(stand_down);
        fleet.sortie().expect("sortie")
    };

    assert_eq!(sortie.grounded, Grounded::StoodDown);
    assert!(sortie.flown.is_empty());
    assert_eq!(provider.remaining(), 4, "a model call was made");
}

/// 🚨 **A verb for the task that just landed does not reach the task that
/// followed it.**
///
/// This is the property the whole design turns on, and it is a property of the
/// *channel* rather than of any desk: a point that outlived its attempt would be
/// a queue the next attempt drains, and `check` latches the first stop it finds.
/// So the verb is sent late — during the **second** task's attempt, naming the
/// **first** — and the assertion is that nothing happened to either.
#[test]
fn a_verb_for_a_landed_task_is_refused_rather_than_delivered_to_the_next_one() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    let first = seed(&mut store, "one returns two");
    let second = seed(&mut store, "and again");
    let repo = Repo::open(&subject.root).expect("open");

    // Four attempts: two for each task. The verb goes out on the third call,
    // which is the first attempt of the *second* task.
    let in_flight = InFlight::default();
    let seen = Arc::new(AtomicUsize::new(0));
    // ⚠ The refusal is counted, not just asserted. Everything else this test
    // checks is also true of a run where the verb was never sent — four attempts,
    // a quiet ending, both tasks in front of a person — so without this the whole
    // test passes whether or not the hook ever fired at the call it names.
    let refused = Arc::new(AtomicUsize::new(0));
    let provider = OnEveryCall::new(absences(4), {
        let in_flight = in_flight.clone();
        let seen = Arc::clone(&seen);
        let refused = Arc::clone(&refused);
        move || {
            if seen.fetch_add(1, Ordering::SeqCst) == 2 {
                assert_eq!(
                    in_flight.deliver(first, abcc_core::event::Control::Kill),
                    Delivery::NotFlying {
                        flying: Some(second)
                    },
                    "a verb for a landed task was accepted while another was flying"
                );
                refused.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    let sortie = {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        )
        .in_flight(in_flight);
        fleet.sortie().expect("sortie")
    };

    // Nothing was stopped: all four attempts flew, and both tasks ended in front
    // of a person because their budgets ran out rather than because of the verb.
    assert_eq!(
        refused.load(Ordering::SeqCst),
        1,
        "the verb was never sent, so this test proved nothing ({} calls seen)",
        seen.load(Ordering::SeqCst)
    );
    assert_eq!(sortie.grounded, Grounded::Quiet);
    assert_eq!(sortie.flown.len(), 4);
    assert!(
        matches!(state(&store, second), TaskState::AwaitingOrders { .. }),
        "the second task took a verb addressed to the first: {:?}",
        state(&store, second)
    );
}

// ---------------------------------------------------------------------------
// A provider that does something while an attempt is running
// ---------------------------------------------------------------------------

/// 🚨 **The operator, standing in for themselves.** A console types while an
/// attempt is running, and a synchronous test has exactly one place it can do
/// the same thing: inside the provider, on the way to a turn.
///
/// ⚠ Without this the timing has to be guessed from another thread, and a test
/// that races the thing it is measuring reports the race.
struct OnFirstCall {
    inner: Scripted,
    once: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl OnFirstCall {
    fn new(scripts: Vec<Script>, act: impl FnOnce() + Send + 'static) -> OnFirstCall {
        OnFirstCall {
            inner: Scripted::new(scripts),
            once: Mutex::new(Some(Box::new(act))),
        }
    }
}

impl Provider for OnFirstCall {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    fn class(&self) -> ProviderClass {
        self.inner.class()
    }

    fn start(
        &self,
        req: &ApiRequest<'_>,
    ) -> std::result::Result<Box<dyn TurnStream>, ProviderError> {
        if let Some(act) = self.once.lock().expect("lock").take() {
            act();
        }
        self.inner.start(req)
    }
}

/// The same, on every call, so a test can pick which one it acts at.
struct OnEveryCall {
    inner: Scripted,
    act: Box<dyn Fn() + Send + Sync>,
}

impl OnEveryCall {
    fn new(scripts: Vec<Script>, act: impl Fn() + Send + Sync + 'static) -> OnEveryCall {
        OnEveryCall {
            inner: Scripted::new(scripts),
            act: Box::new(act),
        }
    }
}

impl Provider for OnEveryCall {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    fn class(&self) -> ProviderClass {
        self.inner.class()
    }

    fn start(
        &self,
        req: &ApiRequest<'_>,
    ) -> std::result::Result<Box<dyn TurnStream>, ProviderError> {
        (self.act)();
        self.inner.start(req)
    }
}

// ---------------------------------------------------------------------------
// The slot's ceiling
// ---------------------------------------------------------------------------

/// 🚨 **The slot cap, end to end: `Fleet` → `Driver` → `TurnLoop` → the request
/// on the wire.**
///
/// Every other test of the ceiling is against `Head::posted` directly, which
/// proves the composition and not the plumbing. This one sets the cap on the
/// fleet, flies one attempt, and reads what the provider was actually handed
/// — because a ceiling that never reaches the request is a setting, not a
/// control.
#[test]
fn a_capped_slot_reaches_the_request_the_provider_is_handed() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    seed(&mut store, "one returns two");
    let repo = Repo::open(&subject.root).expect("open");

    let provider = Scripted::new(absences(2));
    {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        )
        .ceiling(Tier::Read);
        assert_eq!(fleet.slot_ceiling(), Tier::Read);
        fleet.sortie().expect("sortie");
    }

    let seen = provider.seen();
    assert!(!seen.is_empty(), "the provider was never asked anything");
    for call in &seen {
        assert_eq!(call.ceiling, Tier::Read, "{} ran uncapped", call.head_key);
        // 🚨 The half a policy check cannot see: the model was never *told* it
        // had the exec class, so it has nothing to be refused for asking for.
        for name in ["bash", "run_tests", "diagnostics", "git", "write_file"] {
            let declaration = format!("\n{name} — ");
            assert!(
                !call.head_prefix.contains(&declaration),
                "{} advertises {name} from a read-capped slot",
                call.head_key
            );
        }
        assert!(
            call.head_prefix.contains("\nread_file — "),
            "{} lost its read-only tools too",
            call.head_key
        );
    }

    // ⚠ And the log says so. A capped run and an uncapped one differ in the
    // prompt; without this they would agree in every event, and status is a
    // projection of the log alone.
    let ceilings: Vec<String> = store
        .read_from(Seq::new(0), 10_000)
        .expect("history")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::ModelCallStarted { ceiling, .. } => Some(ceiling),
            _ => None,
        })
        .collect();
    assert_eq!(ceilings.len(), seen.len());
    assert!(
        ceilings.iter().all(|c| c == "read"),
        "the log does not record the ceiling in force: {ceilings:?}"
    );
}

/// An unset ceiling is [`Tier::Exec`], so a fleet nobody configured behaves
/// exactly as it did before the cap existed — `Builders` still reaches the exec
/// class, and the roles that never did still do not.
#[test]
fn an_unset_ceiling_changes_nothing() {
    let subject = subject();
    let mut store = Store::in_memory().expect("store");
    seed(&mut store, "one returns two");
    let repo = Repo::open(&subject.root).expect("open");

    let provider = Scripted::new(absences(2));
    {
        let mut fleet = Fleet::new(
            &mut store,
            &repo,
            &provider,
            MODEL,
            subject.worktrees.clone(),
        );
        assert_eq!(fleet.slot_ceiling(), Tier::Exec);
        fleet.sortie().expect("sortie");
    }

    for call in provider.seen() {
        let expected = match call.head_key {
            "engineering" | "recon" => Tier::Read,
            "builders" => Tier::Exec,
            "commandos" => Tier::NoTools,
            other => panic!("an unknown head reached the provider: {other}"),
        };
        assert_eq!(call.ceiling, expected, "{}", call.head_key);
    }
}
