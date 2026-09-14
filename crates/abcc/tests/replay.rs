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
            seed: 0,
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

// ---------------------------------------------------------------------------
// W13's ladder, on the page
// ---------------------------------------------------------------------------

/// ⚠ **An empty ladder is printed as an absence**, not as a zero.
///
/// This is F721's shape at the report layer: `0.0 min / 0` is a reading, and a
/// reading is what a person quotes. *Nothing recorded* is the true statement,
/// and the page has to be the thing that says it.
#[test]
fn the_board_says_the_ladder_is_empty_rather_than_reporting_zero_minutes() {
    let subject = subject();
    {
        let mut store = subject.store();
        store
            .append(Event::MissionCreated {
                title: "m".to_owned(),
            })
            .expect("mission");
        a_truncated_attempt(&mut store, "smoke one");
    }

    let page = subject.run(Command::Replay { task: None }).expect("replay");
    assert!(
        page.contains("W13's ladder"),
        "the archive names the measurement even when it has none of it:\n{page}"
    );
    assert!(page.contains("nothing recorded"), "{page}");
    assert!(
        !page.contains("0.0 min"),
        "🚨 an absence rendered as a zero is a measurement nobody took:\n{page}"
    );
}

/// 🚨 **The ladder on the page: per change, with the passes beside it and the
/// denominator it does not have said out loud.**
///
/// Three recordings over two changes. The page has to report **two**, because
/// the ladder is minutes per *merged change* — counting the events would show
/// the burden falling as a reward for reviewing more.
#[test]
fn the_board_prints_the_ladder_per_change_and_admits_it_has_no_denominator() {
    let subject = subject();
    for (change, seconds, by, boundary) in [
        ("223ae3a", 600, Some("david"), true),
        // The same change, read again by somebody else, with the flag left off.
        ("223ae3a", 300, None, false),
        ("d6c60e0", 120, Some("david"), false),
    ] {
        subject
            .run(Command::Review {
                change: change.to_owned(),
                seconds,
                by: by.map(str::to_owned),
                crossed_boundary: boundary,
            })
            .expect("review");
    }

    let page = subject.run(Command::Replay { task: None }).expect("replay");
    assert!(
        page.contains("2 change(s) · 3 recording(s)"),
        "🚨 two changes read three times, and both numbers are on the page:\n{page}"
    );
    assert!(
        !page.contains("3 change(s)"),
        "🚨 a second pass over one change is not a second change:\n{page}"
    );
    assert!(
        page.contains("17.0 min total"),
        "the minutes sum across every recording:\n{page}"
    );
    assert!(
        page.contains("8.5 min median"),
        "and the median is per change — 15.0 and 2.0 — never per recording:\n{page}"
    );
    assert!(
        page.contains("1 crossed a module boundary"),
        "M3 counts those, so the board does:\n{page}"
    );
    assert!(
        page.contains("2 passes"),
        "the row says the change was read twice:\n{page}"
    );
    assert!(
        page.contains("david, operator") || page.contains("operator, operator"),
        "and by whom, distinct and in order:\n{page}"
    );
    assert!(
        page.contains("no denominator"),
        "🚨 the one reading this number invites is the one it cannot support:\n{page}"
    );
}

/// 🚨🚨 **F744's own closing complaint, answered: `abcc replay` can say it.**
///
/// The archive's 74 cut calls sit on 12 attempts and every one of them replays
/// today without a word about it, because the only witness — a prompt count that
/// fell inside one phase — was never read. ⚠ And the negative half is the half
/// that keeps the line honest: an attempt with nothing recorded prints
/// **nothing** rather than `0 cuts`, because over this archive zero means
/// *nobody was looking*.
#[test]
fn a_cut_prompt_is_printed_and_an_attempt_with_none_recorded_says_nothing() {
    let subject = subject();
    let (task, clean) = {
        let mut store = subject.store();
        store
            .append(Event::MissionCreated {
                title: "m".to_owned(),
            })
            .expect("mission");
        let (task, attempt) = a_truncated_attempt(&mut store, "abcc --version");
        store
            .append(Event::PromptCut {
                attempt,
                reported: 19_181,
                high_water: 36_737,
            })
            .expect("prompt cut");
        let clean = a_truncated_attempt(&mut store, "abcc replay").0;
        (task, clean)
    };

    let page = subject
        .run(Command::Replay {
            task: Some(TaskRef(task.born().get())),
        })
        .expect("replay");
    assert!(
        page.contains("1 call(s) were shown a CUT prompt"),
        "the cut is not on the page:\n{page}"
    );
    assert!(
        page.contains("17556"),
        "and the floor under what went with it:\n{page}"
    );

    let quiet = subject
        .run(Command::Replay {
            task: Some(TaskRef(clean.born().get())),
        })
        .expect("replay");
    assert!(
        !quiet.contains("CUT prompt"),
        "an attempt with nothing recorded printed a reading anyway:\n{quiet}"
    );
}

// ---------------------------------------------------------------------------
// W13's rung, on the page
// ---------------------------------------------------------------------------

/// 🚨 **The archive answers *which rung is this project on?* even when the
/// answer is "none of them".**
///
/// This is A3's whole point. `PLAN.md` puts Self-Host's exit at W13's M3, and
/// for eleven thousand events the only instrument that reads the whole log
/// could not name the criterion, let alone measure it.
#[test]
fn an_empty_ladder_still_names_what_the_rung_would_take() {
    let subject = subject();
    {
        let mut store = subject.store();
        store
            .append(Event::MissionCreated {
                title: "m".to_owned(),
            })
            .expect("mission");
        a_truncated_attempt(&mut store, "smoke one");
    }

    let page = subject.run(Command::Replay { task: None }).expect("replay");
    assert!(
        page.contains(
            "M3 wants 10 boundary-crossing changes, minutes flat or falling. \
             There are none.",
        ),
        "the criterion is on the page before any row is, as one sentence:\n{page}"
    );
}

/// 🚨 **Ten falling crossings print MET — and print the two clauses of M3 that
/// nothing in this workspace writes, in the same breath.**
///
/// *Ten changes, falling* is exactly the sentence a reader finishes as *so M3
/// is reached*. Clause 3 (no human edit to the agent's diff) and clause 4
/// (defect survival at 30 and 90 days, OQ-W13-3) have no writer, so the page
/// that states the half has to state the half it is.
#[test]
fn the_countable_half_is_printed_with_the_half_it_is_not() {
    let subject = subject();
    for (i, seconds) in (1..=10).map(|i| (i, 1100 - i * 100)) {
        subject
            .run(Command::Review {
                change: format!("sha{i}"),
                seconds,
                by: Some("david".to_owned()),
                crossed_boundary: true,
            })
            .expect("review");
    }

    let page = subject.run(Command::Replay { task: None }).expect("replay");
    assert!(
        page.contains("MET — 10 of 10 crossing(s), falling at -100.000 s per change"),
        "the exact slope, because the arithmetic is exact:\n{page}"
    );
    assert!(
        page.contains("Mann–Kendall S -45 · exact two-sided p < 0.001"),
        "the strength beside the direction, never in place of it:\n{page}"
    );
    assert!(
        page.contains(
            "M3's countable half — 10 consecutive boundary-crossing change(s), \
             minutes flat or falling",
        ),
        "the heading names the criterion in full:\n{page}"
    );
    assert!(
        page.contains("two of M3's four clauses"),
        "🚨 the countable half must never print as the milestone:\n{page}"
    );
    assert!(
        page.contains("whether a human edited the agent's diff"),
        "{page}"
    );
    assert!(page.contains("30 and 90 days"), "{page}");
}

/// ⚠ A change nobody called a boundary crossing does not break the run, and the
/// page hands the reader the count that the stricter reading would turn on.
#[test]
fn the_page_reports_what_the_stricter_reading_of_consecutive_would_refuse() {
    let subject = subject();
    for (i, seconds, crossed) in [
        (1, 900, true),
        (2, 60, false),
        (3, 300, true),
        (4, 100, true),
    ] {
        subject
            .run(Command::Review {
                change: format!("sha{i}"),
                seconds,
                by: Some("david".to_owned()),
                crossed_boundary: crossed,
            })
            .expect("review");
    }

    let page = subject.run(Command::Replay { task: None }).expect("replay");
    assert!(page.contains("not met — 3 of 10 crossing(s)"), "{page}");
    assert!(
        page.contains(
            "1 change(s) between them crossed no boundary. They are not M2 tasks \
             so they do not",
        ),
        "the input to the other reading, handed over rather than resolved:\n{page}"
    );
}
