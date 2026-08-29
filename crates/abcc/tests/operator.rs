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
