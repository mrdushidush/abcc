//! `abcc replay` over a real log on disk, asserted on **what it printed**.
//!
//! 🚨 `crates/abcc-core/tests/replay.rs` folds hand-built `Logged` values, which
//! proves the arithmetic and nothing about the report. This proves the other
//! half, and it is the half this project has been caught by: **a test that
//! asserts the inputs to a rendering cannot see a defect in the rendering**
//! (F600 — fifteen passing tests over a sprite placement, and decoding the drawn
//! frame found two defects in it). So every assertion here is on the text a
//! person reads.
//!
//! What that catches which the fold's tests cannot: a number formatted with the
//! wrong unit, a field computed and never printed, a line that renders an
//! absence as a zero, and the two states collapsing into one row *on the page*
//! rather than in the `Vec`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;

use abcc::cli::{Command, Invocation, TaskRef};
use abcc::{AppError, Home};
use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::{CallShape, Composition, Event, Finish, Usage};
use abcc_core::outcome::Why;
use abcc_core::seq::{AttemptId, Seq, TaskId, UnitId};
use abcc_core::task::Command as Lifecycle;
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
    fn run(&self, command: Command) -> Result<String, AppError> {
        let mut out = Vec::new();
        abcc::dispatch(
            &Invocation {
                repo: Some(self.root.clone()),
                home: Some(self.home.clone()),
                command,
            },
            &mut out,
        )?;
        Ok(String::from_utf8(out).expect("utf-8"))
    }

    fn store(&self) -> Store {
        let home = Home::resolve(Some(self.home.clone()), &self.root).expect("home");
        Store::open(&home.log()).expect("open")
    }
}

/// A task that has run one attempt and failed, with the shape this project's own
/// log is full of: a full-cap truncation whose one tool call was thrown away.
///
/// Returns the task and the attempt.
fn a_truncated_attempt(store: &mut Store, title: &str) -> (TaskId, AttemptId) {
    let created = store
        .append(Event::TaskCreated {
            mission: abcc_core::seq::MissionId::at(Seq::new(1)),
            title: title.to_owned(),
            prompt: "add a --version flag".to_owned(),
        })
        .expect("task created");
    let task = TaskId::at(created.seq);
    store
        .apply(task, Lifecycle::Deploy { unit: UnitId(0) })
        .expect("deploy");
    let started = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: Cause::Fresh,
            checkpoint_from: None,
        })
        .expect("attempt started");
    let attempt = AttemptId::at(started.seq);
    store
        .apply(task, Lifecycle::Engage { attempt })
        .expect("engage");
    store
        .append(Event::ModelCallStarted {
            attempt,
            provider: "local".to_owned(),
            model: "champion".to_owned(),
            head: "Builders".to_owned(),
            head_digest: String::new(),
            ceiling: String::new(),
            budget: 8192,
        })
        .expect("call started");
    store
        .append(Event::ModelCallEnded {
            attempt,
            usage: Usage {
                prompt_tokens: 6179,
                completion_tokens: 8192,
                reasoning_tokens: Some(96),
                cached_tokens: None,
            },
            finish: Finish::Length {
                content_empty: false,
            },
            ttfb_ms: 2_700,
            elapsed_ms: 60_000,
            composition: Some(Composition {
                text_chars: 123,
                reasoning_chars: 344,
                calls: vec![CallShape {
                    tool: "apply_patch".to_owned(),
                    argument_chars: 0,
                    arguments: Some(String::new()),
                }],
            }),
        })
        .expect("call ended");
    store
        .append(Event::AttemptEnded {
            task,
            attempt,
            outcome: AttemptOutcome::Uncertain {
                why: Why::TruncatedAtCap { budget: 8192 },
            },
        })
        .expect("attempt ended");
    store
        .apply(task, Lifecycle::Fail { attempt })
        .expect("fail");
    (task, attempt)
}

// ---------------------------------------------------------------------------
// the whole board
// ---------------------------------------------------------------------------

/// 🚨 **The report this view exists for, read off the page.** Two tasks reach
/// the same one-word state by two different endings, and the page has to show
/// one row with both endings under it — and say how much of it is `Uncertain`.
#[test]
fn one_word_on_the_board_prints_the_endings_underneath_it() {
    let subject = subject();
    {
        let mut store = subject.store();
        store
            .append(Event::MissionCreated {
                title: "m".to_owned(),
            })
            .expect("mission");
        a_truncated_attempt(&mut store, "smoke one");

        // A second failure, by a different route, so the row has to hold two.
        let created = store
            .append(Event::TaskCreated {
                mission: abcc_core::seq::MissionId::at(Seq::new(1)),
                title: "smoke two".to_owned(),
                prompt: "again".to_owned(),
            })
            .expect("task");
        let task = TaskId::at(created.seq);
        store
            .apply(task, Lifecycle::Deploy { unit: UnitId(0) })
            .expect("deploy");
        let started = store
            .append(Event::AttemptStarted {
                task,
                unit: UnitId(0),
                cause: Cause::Fresh,
                checkpoint_from: None,
            })
            .expect("started");
        let attempt = AttemptId::at(started.seq);
        store
            .apply(task, Lifecycle::Engage { attempt })
            .expect("engage");
        store
            .append(Event::AttemptEnded {
                task,
                attempt,
                outcome: AttemptOutcome::HardFailure {
                    why: Why::EngineError {
                        detail: "HTTP 500".to_owned(),
                    },
                },
            })
            .expect("ended");
        store
            .apply(task, Lifecycle::Fail { attempt })
            .expect("fail");
    }

    let page = subject.run(Command::Replay { task: None }).expect("replay");

    assert!(
        page.contains("MISSION FAILED"),
        "it names the state with the word the operator saw:\n{page}"
    );
    assert_eq!(
        page.matches("MISSION FAILED").count(),
        1,
        "two failed tasks are ONE row on the page, not two:\n{page}"
    );
    assert!(
        page.contains("2 task(s), 2 attempt(s)"),
        "and the row counts both:\n{page}"
    );
    assert!(
        page.contains("Uncertain/TruncatedAtCap"),
        "the first ending is printed:\n{page}"
    );
    assert!(
        page.contains("HardFailure/EngineError"),
        "and so is the second, which is the whole point:\n{page}"
    );
    assert!(
        page.contains("1 of 2 are Uncertain"),
        "🚨 and the page says how much of that one word is `could not tell`:\n{page}"
    );
}

/// An empty log says what to type next rather than printing an empty table.
#[test]
fn an_empty_log_says_what_to_do() {
    let subject = subject();
    let page = subject.run(Command::Replay { task: None }).expect("replay");
    assert!(page.contains("the log is empty"), "{page}");
}

// ---------------------------------------------------------------------------
// one task
// ---------------------------------------------------------------------------

/// 🚨 **The diagnosis, read off the page.** Every one of these numbers is
/// computed somewhere the fold's tests already cover — what this asserts is that
/// it reaches the operator, in the unit they can act on.
#[test]
fn one_task_prints_its_spend_its_discard_and_its_ending() {
    let subject = subject();
    let task = {
        let mut store = subject.store();
        store
            .append(Event::MissionCreated {
                title: "m".to_owned(),
            })
            .expect("mission");
        a_truncated_attempt(&mut store, "abcc --version").0
    };

    let page = subject
        .run(Command::Replay {
            task: Some(TaskRef(task.born().get())),
        })
        .expect("replay");

    assert!(page.contains("MISSION FAILED"), "{page}");
    assert!(page.contains("abcc --version"), "the title:\n{page}");
    // The lifecycle, in order.
    assert!(page.contains("Queued -> Deployed"), "{page}");
    assert!(page.contains("Engaged -> Failed"), "{page}");

    // 🚨 F511: the composition against what the server billed. Printing the
    // composition alone would read as a complete account of the completion.
    assert!(
        page.contains("467 char(s)"),
        "the composition's total is printed:\n{page}"
    );
    assert!(
        page.contains("8192 completion token(s)"),
        "and so is what was billed:\n{page}"
    );
    assert!(
        page.contains("accounts for ~116 (1%)"),
        "🚨 and the fraction, which is the finding:\n{page}"
    );

    // 🚨 F506: the log is the only copy of a thrown-away call's arguments, and an
    // empty one must not render as a blank.
    assert!(
        page.contains("discarded apply_patch"),
        "the thrown-away call is named:\n{page}"
    );
    assert!(
        page.contains("cut before a single argument character arrived"),
        "🚨 and an empty argument string says so rather than printing nothing:\n{page}"
    );

    assert!(
        page.contains("Uncertain — stopped at the 8192-token cap"),
        "the ending carries its `Why`:\n{page}"
    );
}

/// A task the log never created is refused with the command that would list the
/// ones it did, rather than an empty report.
#[test]
fn a_task_that_was_never_created_is_refused() {
    let subject = subject();
    {
        let mut store = subject.store();
        store
            .append(Event::MissionCreated {
                title: "m".to_owned(),
            })
            .expect("mission");
        a_truncated_attempt(&mut store, "real");
    }

    let Err(refused) = subject.run(Command::Replay {
        task: Some(TaskRef(9_999)),
    }) else {
        panic!("a task the log never created must be refused");
    };
    let said = refused.to_string();
    assert!(said.contains("t9999"), "{said}");
    assert!(said.contains("abcc replay"), "{said}");
    assert_eq!(refused.exit_code(), 1);
}

/// ⚠ A task that never ran an attempt says so. It is the case a report that
/// only ever prints attempts renders as a blank page.
#[test]
fn a_task_with_no_attempt_says_so() {
    let subject = subject();
    let task = {
        let mut store = subject.store();
        store
            .append(Event::MissionCreated {
                title: "m".to_owned(),
            })
            .expect("mission");
        let created = store
            .append(Event::TaskCreated {
                mission: abcc_core::seq::MissionId::at(Seq::new(1)),
                title: "never run".to_owned(),
                prompt: "later".to_owned(),
            })
            .expect("task");
        TaskId::at(created.seq)
    };

    let page = subject
        .run(Command::Replay {
            task: Some(TaskRef(task.born().get())),
        })
        .expect("replay");
    assert!(page.contains("no attempt has ever started"), "{page}");
    assert!(
        page.contains("never left the state it was created in"),
        "{page}"
    );
}
