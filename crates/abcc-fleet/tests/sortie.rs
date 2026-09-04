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
use abcc_core::event::Event;
use abcc_core::seq::{MissionId, Seq, TaskId};
use abcc_core::task::TaskState;
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
