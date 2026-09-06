//! The subcommands a person runs, against a real repository and a real log.
//!
//! 🚨 The claim these exist for is `PLAN.md` §3's Skeleton exit — *one real task
//! runs to a durable terminal state* — and the shape of the answer, which is not
//! the obvious one. `abcc-drive` ends a working attempt
//! `Uncertain { NoCheckerForArtifact }` and leaves the task in `AwaitingOrders`,
//! because there is no gate until the Gate milestone and calling a model's answer
//! `Accomplished` would put a claim where a measurement belongs.
//! **`AwaitingOrders` is not terminal**, so the terminal state is reached by a
//! person: `abcc accept` commandeers the task and finishes it by hand, which is
//! `Aborted { CompletedByOperator }` — the variant that exists for exactly this.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;

use abcc::cli::{Command, Invocation, TaskRef};
use abcc::{Home, ops};
use abcc_core::event::Event;
use abcc_core::seq::{AttemptId, PromptId, Seq, TaskId, UnitId};
use abcc_core::task::{AbortReason, Command as Lifecycle, TaskState};
use abcc_store::Store;

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

fn git(cwd: &Path, args: &[&str]) {
    git_out(cwd, args);
}

/// The same, handing back what git said. Used to read a checkpoint's tree, which
/// is how a test asks *did the operator's work actually land in the snapshot*
/// rather than *did we write an event saying it did*.
fn git_out(cwd: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Subject {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

/// A repository with one commit, and a state directory beside it rather than
/// inside it.
fn subject() -> Subject {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("subject");
    fs::create_dir_all(&root).expect("mkdir");
    git(dir.path(), &["init", "-q", "-b", "main", "subject"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "test"]);
    fs::write(root.join("src.rs"), "pub fn one() -> u32 { 1 }\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "first"]);
    Subject {
        home: dir.path().join("state"),
        _dir: dir,
        root,
    }
}

impl Subject {
    /// An invocation of `command` against this repository, with the state
    /// directory named explicitly so no test depends on the environment.
    fn invoke(&self, command: Command) -> Invocation {
        Invocation {
            repo: Some(self.root.clone()),
            home: Some(self.home.clone()),
            command,
        }
    }

    fn run(&self, command: Command) -> Result<String, abcc::AppError> {
        let mut out = Vec::new();
        abcc::dispatch(&self.invoke(command), &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8"))
    }

    fn store(&self) -> Store {
        let home = Home::resolve(Some(self.home.clone()), &self.root).expect("home");
        Store::open(&home.log()).expect("open")
    }

    fn state(&self, task: TaskId) -> TaskState {
        self.store().task(task).expect("task").expect("row").state
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.store()
            .read_from(Seq::ORIGIN, 1000)
            .expect("read")
            .iter()
            .map(|l| l.event.kind())
            .collect()
    }
}

/// Put a task on the board and return its id.
fn queue(subject: &Subject, prompt: &str) -> TaskId {
    subject
        .run(Command::Task {
            prompt: prompt.to_owned(),
            title: None,
        })
        .expect("task");
    let store = subject.store();
    store.tasks().expect("tasks").last().expect("one task").id
}

/// Walk a task to `Engaged` the way the driver does: a slot, an attempt, and the
/// command that starts it.
fn engaged(subject: &Subject, task: TaskId) -> AttemptId {
    let mut store = subject.store();
    store
        .apply(task, Lifecycle::Deploy { unit: UnitId(0) })
        .expect("deploy");
    let started = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: abcc_core::attempt::Cause::Fresh,
            checkpoint_from: None,
        })
        .expect("attempt");
    let attempt = AttemptId::at(started.seq);
    store
        .apply(task, Lifecycle::Engage { attempt })
        .expect("engage");
    attempt
}

/// Walk a task to `AwaitingOrders` the way the driver does, without a model.
///
/// The lifecycle is the authority on the route, so this sends the same commands
/// in the same order rather than writing a state anywhere.
fn awaiting_orders(subject: &Subject, task: TaskId) -> (AttemptId, PromptId) {
    let attempt = engaged(subject, task);
    let mut store = subject.store();
    let asked = store
        .append(Event::OperatorPrompted {
            task,
            attempt,
            question: "nothing measured it. Read it and say what should happen.".into(),
        })
        .expect("prompt");
    let prompt = PromptId::at(asked.seq);
    store
        .apply(task, Lifecycle::RequestOrders { attempt, prompt })
        .expect("request orders");
    (attempt, prompt)
}

// ---------------------------------------------------------------------------
// the board
// ---------------------------------------------------------------------------

#[test]
fn a_task_lands_queued_and_the_board_shows_it() {
    let subject = subject();
    let said = subject
        .run(Command::Task {
            prompt: "make one() return two".to_owned(),
            title: None,
        })
        .expect("task");
    assert!(said.contains("queued"), "{said}");

    let board = subject.run(Command::Board).expect("board");
    assert!(board.contains("make one() return two"), "{board}");
    assert_eq!(subject.kinds(), vec!["mission_created", "task_created"]);
}

#[test]
fn a_second_task_joins_the_first_mission_rather_than_starting_another() {
    let subject = subject();
    queue(&subject, "first");
    queue(&subject, "second");
    assert_eq!(
        subject.kinds(),
        vec!["mission_created", "task_created", "task_created"]
    );
}

#[test]
fn a_task_with_no_title_is_titled_by_the_first_line_of_its_prompt() {
    let subject = subject();
    let task = queue(&subject, "make one() return two\n\nand nothing else");
    let store = subject.store();
    let row = store.task(task).expect("task").expect("row");
    assert_eq!(row.title, "make one() return two");
    assert!(row.prompt.contains("nothing else"), "the prompt is whole");
}

#[test]
fn a_listing_command_does_not_reconcile_a_live_attempt() {
    // 🚨 `Store::boot`'s orphan sweep tombstones the attempt behind any
    // slot-holding state and requeues its task. That is right for a process that
    // has just started and catastrophic for a second one running beside a live
    // attempt: `abcc board` would kill the run it was opened to look at. Only
    // `abcc run` reconciles.
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    engaged(&subject, task);

    subject.run(Command::Board).expect("board");
    subject.run(Command::Where).expect("where");

    assert!(
        matches!(subject.state(task), TaskState::Engaged { .. }),
        "a listing command moved a live task: {:?}",
        subject.state(task)
    );
    let kinds = subject.kinds();
    assert!(
        !kinds.contains(&"attempt_ended"),
        "a listing command tombstoned a live attempt: {kinds:?}"
    );
}

// ---------------------------------------------------------------------------
// the operator's two endings
// ---------------------------------------------------------------------------

#[test]
fn accepting_lands_a_durable_terminal_state_that_is_not_accomplished() {
    // 🚨 Skeleton's exit criterion, and the honest shape of it.
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    awaiting_orders(&subject, task);

    let said = subject
        .run(Command::Accept {
            task: TaskRef(task.born().get()),
            note: Some("read the diff, it is right".to_owned()),
        })
        .expect("accept");

    let state = subject.state(task);
    assert!(state.is_terminal(), "{state:?} is not a durable ending");
    let TaskState::Aborted {
        reason: AbortReason::CompletedByOperator { .. },
        ..
    } = state
    else {
        panic!("accepting has to be completed-by-operator, and got {state:?}");
    };
    // The sentence an operator reads has to say what was and was not claimed.
    assert!(said.contains("not `Accomplished`"), "{said}");

    // And the route is the honest one: the answer against the standing question,
    // the keyboard taken, then the ending.
    let kinds = subject.kinds();
    let tail = &kinds[kinds.len() - 3..];
    assert_eq!(
        tail,
        [
            "operator_answered",
            "task_transitioned",
            "task_transitioned"
        ]
    );
}

#[test]
fn the_answer_is_recorded_against_the_question_that_was_asked() {
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    let (_, prompt) = awaiting_orders(&subject, task);

    subject
        .run(Command::Accept {
            task: TaskRef(task.born().get()),
            note: Some("good".to_owned()),
        })
        .expect("accept");

    let answered = subject
        .store()
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find_map(|l| match l.event {
            Event::OperatorAnswered { prompt, answer, .. } => Some((prompt, answer)),
            _ => None,
        })
        .expect("an answer");
    assert_eq!(answered, (prompt, "good".to_owned()));
}

#[test]
fn rejecting_stops_the_task_and_is_attributed_to_a_person() {
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    awaiting_orders(&subject, task);

    subject
        .run(Command::Reject {
            task: TaskRef(task.born().get()),
            note: None,
        })
        .expect("reject");

    // ⚠ `Aborted { Operator }` and not `Failed`: `Fail` names an attempt and is
    // legal only from `Engaged`. From `AwaitingOrders` the attempt is already
    // over, and the thing being stopped is the task.
    let state = subject.state(task);
    let TaskState::Aborted {
        reason: AbortReason::Operator { .. },
        ..
    } = state
    else {
        panic!("rejecting has to be an operator abort, and got {state:?}");
    };
    assert!(state.is_terminal());
}

#[test]
fn ending_a_task_that_is_not_waiting_on_anybody_records_a_note_and_not_a_question() {
    // A `PromptId` invented to satisfy the type would put a question on the log
    // that was never asked.
    let subject = subject();
    let task = queue(&subject, "make one() return two");

    subject
        .run(Command::Accept {
            task: TaskRef(task.born().get()),
            note: None,
        })
        .expect("accept");

    let kinds = subject.kinds();
    assert!(!kinds.contains(&"operator_answered"), "{kinds:?}");
    assert!(kinds.contains(&"note"), "{kinds:?}");
    assert!(subject.state(task).is_terminal());
}

#[test]
fn a_task_that_is_not_on_the_board_is_a_sentence_and_not_a_panic() {
    let subject = subject();
    queue(&subject, "make one() return two");
    let refused = subject
        .run(Command::Accept {
            task: TaskRef(9999),
            note: None,
        })
        .expect_err("no such task");
    assert!(refused.to_string().contains("t9999"), "{refused}");
    assert_eq!(refused.exit_code(), 1);
}

#[test]
fn a_terminal_task_cannot_be_ended_twice() {
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    let reference = TaskRef(task.born().get());
    subject
        .run(Command::Accept {
            task: reference,
            note: None,
        })
        .expect("accept");
    let refused = subject
        .run(Command::Reject {
            task: reference,
            note: None,
        })
        .expect_err("already ended");
    assert!(refused.to_string().contains("refused"), "{refused}");
}

// ---------------------------------------------------------------------------
// review — the measurement W13's ladder is defined in
// ---------------------------------------------------------------------------

#[test]
fn a_review_is_recorded_in_seconds_and_says_whether_it_crossed_a_boundary() {
    // 🚨 `PLAN.md` §5 wants this from Skeleton onward. Until this subcommand
    // there was nothing in the workspace that wrote one, and a ladder with no
    // baseline is unfalsifiable.
    let subject = subject();
    subject
        .run(Command::Review {
            change: "a2c0ca3".to_owned(),
            seconds: 510,
            by: Some("david".to_owned()),
            crossed_boundary: true,
        })
        .expect("review");

    let recorded = subject
        .store()
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find_map(|l| match l.event {
            Event::ReviewRecorded {
                change,
                seconds,
                by,
                crossed_boundary,
            } => Some((change, seconds, by, crossed_boundary)),
            _ => None,
        })
        .expect("a review");
    assert_eq!(
        recorded,
        ("a2c0ca3".to_owned(), 510, "david".to_owned(), true)
    );
}

// ---------------------------------------------------------------------------
// where
// ---------------------------------------------------------------------------

#[test]
fn where_names_both_paths_and_neither_is_in_the_repository() {
    let subject = subject();
    let said = subject.run(Command::Where).expect("where");
    let home = Home::resolve(Some(subject.home.clone()), &subject.root).expect("home");
    assert!(said.contains(&home.log().display().to_string()), "{said}");
    assert!(
        said.contains(&home.worktrees().display().to_string()),
        "{said}"
    );
    assert!(
        !home
            .root()
            .starts_with(fs::canonicalize(&subject.root).expect("canonical"))
    );
}

/// The one thing `ops` exposes that is not a subcommand, kept honest.
#[test]
fn the_ground_refuses_a_directory_that_is_not_a_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    // `Ground` holds a `Repo` and is not `Debug`, so the error is taken by hand
    // rather than by `expect_err`.
    let Err(refused) = ops::ground(&Invocation {
        repo: Some(dir.path().to_path_buf()),
        home: Some(dir.path().join("state")),
        command: Command::Board,
    }) else {
        panic!("a bare directory is not a repository");
    };
    assert_eq!(refused.exit_code(), 1);
}

// ---------------------------------------------------------------------------
// take / release — ADR-0012 §4's eighth verb
// ---------------------------------------------------------------------------

/// The worktree the log says is open for a task, folded here rather than read
/// back from `abcc::takeover`.
///
/// 🚨 A second implementation on purpose. F600: a test that asserts the inputs to
/// a thing cannot see a defect in the thing — so the fold these tests check
/// against is not the fold that ships.
fn open_worktree(subject: &Subject, task: TaskId) -> Option<PathBuf> {
    let mut open = None;
    for logged in subject.store().task_history(task).expect("history") {
        match logged.event {
            Event::WorktreeOpened { path, .. } => open = Some(PathBuf::from(path)),
            Event::WorktreeClosed { .. } => open = None,
            _ => {}
        }
    }
    open
}

/// Every checkpoint sha recorded for a task, oldest first.
fn checkpoints(subject: &Subject, task: TaskId) -> Vec<String> {
    subject
        .store()
        .task_history(task)
        .expect("history")
        .into_iter()
        .filter_map(|l| match l.event {
            Event::CheckpointTaken { sha, .. } => Some(sha),
            _ => None,
        })
        .collect()
}

#[test]
fn taking_over_hands_the_operator_the_state_and_a_tree_to_work_in() {
    // 🚨 The half of *take over manually* that was missing. The lifecycle has had
    // `Commandeer` since Skeleton; what an operator could not get was a directory
    // to stand in.
    let subject = subject();
    let task = queue(&subject, "make one() return two");

    let said = subject
        .run(Command::Take {
            task: TaskRef(task.born().get()),
        })
        .expect("take");

    let TaskState::Commandeered { .. } = subject.state(task) else {
        panic!("take has to leave the task under manual control");
    };
    let path = open_worktree(&subject, task).expect("a worktree on the log");
    assert!(path.is_dir(), "{} is not there", path.display());
    assert!(
        path.join("src.rs").is_file(),
        "the tree has the repository in it"
    );
    assert!(said.contains(&path.display().to_string()), "{said}");

    // 🚨 The log first and the directory second. Nothing boots after an operator
    // command, so a task moved with no directory is recoverable and a directory
    // with no task on the log is an orphan nobody can find.
    let kinds = subject.kinds();
    assert_eq!(
        &kinds[kinds.len() - 3..],
        ["task_transitioned", "checkpoint_taken", "worktree_opened"],
        "{kinds:?}"
    );
}

#[test]
fn a_take_over_cuts_the_tree_at_the_last_checkpoint_and_not_at_head() {
    // The tree is *the work as the fleet left it*. A take-over that cut at HEAD
    // would hand the operator a directory with the attempt's work missing — and
    // every assertion about paths and events would still pass.
    let subject = subject();
    let task = queue(&subject, "make one() return two");

    // Stand in for an attempt: change the tree, snapshot it the way the driver
    // does, then put the checkout back.
    fs::write(subject.root.join("src.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let repo = abcc_vcs::Repo::open(&subject.root).expect("repo");
    let git_ref = abcc_vcs::checkpoint_ref("m1", 7);
    let sha = repo
        .checkpoint(&git_ref, "an attempt's closing snapshot")
        .expect("checkpoint");
    fs::write(subject.root.join("src.rs"), "pub fn one() -> u32 { 1 }\n").expect("write back");
    subject
        .store()
        .append(Event::CheckpointTaken {
            task,
            sha: sha.to_string(),
            git_ref,
        })
        .expect("record it");

    subject
        .run(Command::Take {
            task: TaskRef(task.born().get()),
        })
        .expect("take");

    let path = open_worktree(&subject, task).expect("a worktree");
    let content = fs::read_to_string(path.join("src.rs")).expect("read");
    assert!(
        content.contains("{ 2 }"),
        "the tree was cut at HEAD rather than at the checkpoint: {content:?}"
    );
    // And no second snapshot was taken: there was already one to stand on.
    assert_eq!(checkpoints(&subject, task), vec![sha.to_string()]);
}

#[test]
fn taking_over_is_refused_while_the_fleet_holds_the_slot() {
    // 🚨 `Commandeer` is legal from `Engaged` and this verb is not: the
    // transition moves a state and the verb has to move a directory, and that
    // directory belongs to a driver still writing in it.
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    engaged(&subject, task);

    let refused = subject
        .run(Command::Take {
            task: TaskRef(task.born().get()),
        })
        .expect_err("the fleet is holding it");

    let said = refused.to_string();
    assert!(
        said.contains("halt"),
        "the refusal has to name the way out: {said}"
    );
    let TaskState::Engaged { .. } = subject.state(task) else {
        panic!("a refused take may not move the task");
    };
    assert!(
        open_worktree(&subject, task).is_none(),
        "and may not cut a tree"
    );
    assert!(
        !subject.kinds().contains(&"checkpoint_taken"),
        "or take a snapshot"
    );
}

#[test]
fn taking_over_a_finished_task_is_refused_and_sent_to_the_replay() {
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    let reference = TaskRef(task.born().get());
    subject
        .run(Command::Accept {
            task: reference,
            note: None,
        })
        .expect("accept");

    let refused = subject
        .run(Command::Take { task: reference })
        .expect_err("it is over");
    assert!(refused.to_string().contains("replay"), "{refused}");
}

#[test]
fn taking_over_twice_adopts_the_tree_that_is_already_standing() {
    // Idempotence is the small half. The real one: the driver records a worktree
    // it could not remove as a `Note` and leaves the directory there, so the tree
    // that is standing is sometimes fresher than the last checkpoint.
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    let reference = TaskRef(task.born().get());

    subject
        .run(Command::Take { task: reference })
        .expect("take");
    let first = open_worktree(&subject, task).expect("a worktree");
    fs::write(first.join("mine.txt"), "the operator was here\n").expect("write");

    let said = subject
        .run(Command::Take { task: reference })
        .expect("take again");

    assert_eq!(open_worktree(&subject, task).as_ref(), Some(&first));
    assert!(
        first.join("mine.txt").is_file(),
        "the operator's work is still there"
    );
    assert!(said.contains("already standing"), "{said}");
    assert_eq!(
        subject
            .kinds()
            .iter()
            .filter(|k| **k == "worktree_opened")
            .count(),
        1,
        "a second take may not cut a second tree"
    );
    assert_eq!(
        checkpoints(&subject, task).len(),
        1,
        "nor take a second snapshot"
    );
}

#[test]
fn releasing_snapshots_what_the_operator_did_and_hands_the_task_back() {
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    let reference = TaskRef(task.born().get());
    subject
        .run(Command::Take { task: reference })
        .expect("take");
    let path = open_worktree(&subject, task).expect("a worktree");
    fs::write(path.join("src.rs"), "pub fn one() -> u32 { 2 }\n").expect("the operator works");

    let said = subject
        .run(Command::Release { task: reference })
        .expect("release");

    assert_eq!(subject.state(task), TaskState::Queued);
    assert!(!path.exists(), "the tree is down: {}", path.display());
    assert!(
        open_worktree(&subject, task).is_none(),
        "and the log says so"
    );

    // 🚨 The claim worth testing is not that an event was written — it is that the
    // work is in the object database and outlives the directory.
    let closing = checkpoints(&subject, task)
        .pop()
        .expect("a closing snapshot");
    let kept = git_out(&subject.root, &["show", &format!("{closing}:src.rs")]);
    assert!(
        kept.contains("{ 2 }"),
        "the operator's work is not in the snapshot: {kept:?}"
    );
    assert!(said.contains(&closing), "{said}");
}

#[test]
fn releasing_a_task_nobody_took_over_is_refused_before_anything_is_touched() {
    // 🚨 The order matters more than the refusal. `hand_back` snapshots a tree and
    // removes it; running it before the legality check would take the *driver's*
    // worktree down under a live attempt and only then discover that `Release` is
    // not legal from `Engaged`.
    //
    // ⚠ F586's shape: with no worktree on the log there is nothing for an
    // unguarded `hand_back` to destroy, so the test would pass having measured
    // nothing. The attempt gets a real tree, the way the driver would.
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    engaged(&subject, task);
    let repo = abcc_vcs::Repo::open(&subject.root).expect("repo");
    let head = repo.head().expect("head");
    let flying = subject.home.join("the-drivers-tree");
    repo.open_worktree(&flying, &head)
        .expect("the driver cuts its own");
    subject
        .store()
        .append(Event::WorktreeOpened {
            task,
            path: flying.display().to_string(),
            sha: head.to_string(),
        })
        .expect("record it");
    let before = subject.kinds();

    let refused = subject
        .run(Command::Release {
            task: TaskRef(task.born().get()),
        })
        .expect_err("nobody took it over");

    assert!(refused.to_string().contains("abcc take"), "{refused}");
    assert_eq!(subject.kinds(), before, "a refused release writes nothing");
    assert!(
        flying.is_dir(),
        "and takes down no tree it does not own: {}",
        flying.display()
    );
}

#[test]
fn a_task_may_not_go_terminal_still_holding_a_workspace() {
    // 🚨 All three terminal states say `holds_workspace: false` on their contract.
    // Until `abcc take` there was no way to break that claim, because the driver
    // closes its own worktree before it sends the landing command.
    for finish in ["accept", "reject"] {
        let subject = subject();
        let task = queue(&subject, "make one() return two");
        let reference = TaskRef(task.born().get());
        subject
            .run(Command::Take { task: reference })
            .expect("take");
        let path = open_worktree(&subject, task).expect("a worktree");
        fs::write(path.join("src.rs"), "pub fn one() -> u32 { 2 }\n").expect("the operator works");

        let said = subject
            .run(if finish == "accept" {
                Command::Accept {
                    task: reference,
                    note: None,
                }
            } else {
                Command::Reject {
                    task: reference,
                    note: None,
                }
            })
            .expect(finish);

        let state = subject.state(task);
        assert!(state.is_terminal(), "{finish}: {state:?}");
        assert!(!state.contract().holds_workspace, "{finish}: {state:?}");
        assert!(!path.exists(), "{finish} left {} standing", path.display());
        assert!(open_worktree(&subject, task).is_none(), "{finish}");

        let closing = checkpoints(&subject, task)
            .pop()
            .expect("a closing snapshot");
        let kept = git_out(&subject.root, &["show", &format!("{closing}:src.rs")]);
        assert!(
            kept.contains("{ 2 }"),
            "{finish} threw the work away: {kept:?}"
        );
        assert!(said.contains(&closing), "{finish}: {said}");
    }
}

#[test]
fn a_workspace_the_operator_deleted_is_closed_on_the_log_rather_than_failing() {
    let subject = subject();
    let task = queue(&subject, "make one() return two");
    let reference = TaskRef(task.born().get());
    subject
        .run(Command::Take { task: reference })
        .expect("take");
    let path = open_worktree(&subject, task).expect("a worktree");
    fs::remove_dir_all(&path).expect("the operator tidies up by hand");

    subject
        .run(Command::Release { task: reference })
        .expect("release still works");

    assert_eq!(subject.state(task), TaskState::Queued);
    assert!(open_worktree(&subject, task).is_none());
    assert!(
        subject.kinds().contains(&"note"),
        "and it says what happened"
    );
}
