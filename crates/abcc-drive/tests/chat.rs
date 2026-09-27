//! `Driver::chat` — one attempt as a conversation, against real git and a
//! scripted model. The fixture is `attempt.rs`'s, cut down.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;

use abcc_core::attempt::Cause;
use abcc_core::event::{Control, Event};
use abcc_core::outcome::Reading;
use abcc_core::run::AttemptPhase;
use abcc_core::seq::{MissionId, TaskId, UnitId};
use abcc_core::task::TaskState;
use abcc_drive::{Driver, Landed, Operator, Said};
use abcc_engine::control::ControlHandle;
use abcc_engine::provider::{Body, Delta, Role};
use abcc_engine::scripted::{Script, Scripted};
use abcc_engine::turn::PhaseEnded;
use abcc_engine::workspace::Toolchain;
use abcc_store::Store;
use abcc_vcs::Repo;

const MODEL: &str = "qwen3.6-35b-a3b-mtp@iq3_s";

fn git(cwd: &Path, args: &[&str]) {
    let out = OsCommand::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Subject {
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

fn seed(store: &mut Store) -> TaskId {
    let m = store
        .append(Event::MissionCreated {
            title: "chat".into(),
        })
        .expect("mission");
    let t = store
        .append(Event::TaskCreated {
            mission: MissionId::at(m.seq),
            title: "add two".into(),
            prompt: "add a function two() that returns 2".into(),
        })
        .expect("task");
    TaskId::at(t.seq)
}

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

/// What the Judge answers when it is asked.
const REVIEWED: &str = r#"{"assessment":"It adds src/new.rs.","findings":[]}"#;

fn writes() -> Script {
    Script::calls(
        "w1",
        "write_file",
        r#"{"path":"src/new.rs","content":"pub fn two() -> u32 { 2 }\n"}"#,
    )
}

/// An operator reading from a script, and remembering what it was shown.
#[derive(Default)]
struct Typist {
    says: VecDeque<Said>,
    /// Interrupt every turn the moment it starts.
    interrupt: bool,
    turns: Vec<&'static str>,
    diffs: Vec<String>,
    notes: Vec<String>,
}

impl Operator for Typist {
    fn next(&mut self) -> Said {
        self.says.pop_front().unwrap_or(Said::Quit)
    }
    fn turn_starting(&mut self, interrupt: ControlHandle) {
        if self.interrupt {
            interrupt.request(Control::Halt).expect("halt");
        }
    }
    fn event(&mut self, _event: &Event) {}
    fn turn_ended(&mut self, ended: &PhaseEnded) {
        self.turns.push(match ended {
            PhaseEnded::Answered { .. } => "answered",
            PhaseEnded::Stopped { .. } => "stopped",
            PhaseEnded::Unmeasured { .. } => "unmeasured",
        });
    }
    fn diff(&mut self, patch: &str) {
        self.diffs.push(patch.to_owned());
    }
    fn note(&mut self, text: &str) {
        self.notes.push(text.to_owned());
    }
}

// One call site per test reads better than a builder for a test helper.
#[allow(clippy::too_many_arguments)]
fn chat(
    subject: &Subject,
    store: &mut Store,
    task: TaskId,
    cause: Cause,
    body: &mut Body,
    provider: &Scripted,
    operator: &mut Typist,
    toolchain: Toolchain,
) -> Landed {
    let repo = Repo::open(&subject.root).expect("open");
    let quiet = |_: &Delta| {};
    Driver::new(store, &repo, provider, MODEL, &subject.worktrees)
        .toolchain(toolchain)
        .retry_available(true)
        .chat(task, UnitId(0), cause, body, operator, &quiet)
        .expect("chat")
}

fn events(store: &Store) -> Vec<Event> {
    store
        .read_from(abcc_core::seq::Seq::new(0), 10_000)
        .expect("read")
        .into_iter()
        .map(|l| l.event)
        .collect()
}

/// One conversation under Builders: the brief goes in, the model writes and
/// answers, `/done` runs the gate and a green tree lands `Accomplished`. No
/// Localize phase is entered.
#[test]
fn a_chat_works_from_the_brief_and_done_lands_a_green_tree() {
    let s = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);
    let provider = Scripted::new(vec![
        writes(),
        Script::says("added two()"),
        Script::says(REVIEWED),
    ]);
    let mut operator = Typist {
        says: VecDeque::from([Said::Diff, Said::Done]),
        ..Typist::default()
    };
    let mut body = Body::new();

    let landed = chat(
        &s,
        &mut store,
        task,
        Cause::Fresh,
        &mut body,
        &provider,
        &mut operator,
        PASSING,
    );

    assert!(
        matches!(landed.state, TaskState::Accomplished { .. }),
        "{:?}",
        landed.state
    );
    assert_eq!(operator.turns, vec!["answered"]);
    assert!(
        operator.diffs[0].contains("src/new.rs"),
        "{}",
        operator.diffs[0]
    );
    let phases: Vec<AttemptPhase> = events(&store)
        .into_iter()
        .filter_map(|e| match e {
            Event::AttemptPhaseEntered { phase, .. } => Some(phase),
            _ => None,
        })
        .collect();
    assert!(!phases.contains(&AttemptPhase::Localize), "{phases:?}");
    assert!(body.messages()[0].content.contains("add a function two()"));
}

/// What the operator types reaches the model as a user message and the log as
/// `OperatorSaid`, and the conversation keeps everything before it.
#[test]
fn what_the_operator_says_reaches_the_model_and_the_log() {
    let s = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);
    let provider = Scripted::new(vec![
        Script::says("where should it go?"),
        writes(),
        Script::says("done"),
        Script::says(REVIEWED),
    ]);
    let mut operator = Typist {
        says: VecDeque::from([Said::Ask("put it in src/new.rs".into()), Said::Done]),
        ..Typist::default()
    };
    let mut body = Body::new();

    chat(
        &s,
        &mut store,
        task,
        Cause::Fresh,
        &mut body,
        &provider,
        &mut operator,
        PASSING,
    );

    let second = &provider.seen()[1];
    let last_user = second
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .expect("a user message");
    assert_eq!(last_user.content, "put it in src/new.rs");
    assert_eq!(
        second.messages[0].content,
        body.messages()[0].content,
        "the brief was kept"
    );
    assert!(events(&store).iter().any(|e| matches!(
        e,
        Event::OperatorSaid { text, .. } if text.as_str() == "put it in src/new.rs"
    )));
}

/// `/quit` keeps the work: the task holds at a checkpoint, and no gate ran.
#[test]
fn quit_keeps_the_work_and_the_task_holds() {
    let s = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);
    let provider = Scripted::new(vec![writes(), Script::says("added it")]);
    let mut operator = Typist {
        says: VecDeque::from([Said::Quit]),
        ..Typist::default()
    };
    let mut body = Body::new();

    let landed = chat(
        &s,
        &mut store,
        task,
        Cause::Fresh,
        &mut body,
        &provider,
        &mut operator,
        PASSING,
    );

    assert!(
        matches!(landed.state, TaskState::Holding { .. }),
        "{:?}",
        landed.state
    );
    assert!(landed.kept.is_some());
    assert!(landed.gate.is_none());
}

/// A red gate hands the task to the operator; at the chat they say keep going,
/// and the next attempt carries the whole conversation plus what the check said.
#[test]
fn a_refused_tree_carries_on_and_the_next_attempt_is_told_why() {
    let s = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);
    let provider = Scripted::new(vec![
        writes(),
        Script::says("added it"),
        Script::says(REVIEWED),
        Script::says("I see the test failure"),
    ]);
    let mut body = Body::new();
    let mut first = Typist {
        says: VecDeque::from([Said::Done]),
        ..Typist::default()
    };
    let landed = chat(
        &s,
        &mut store,
        task,
        Cause::Fresh,
        &mut body,
        &provider,
        &mut first,
        FAILING,
    );
    assert!(
        matches!(landed.state, TaskState::AwaitingOrders { .. }),
        "a red gate asks the operator: {:?}",
        landed.state
    );
    let repo = Repo::open(&s.root).expect("open");
    let held = Driver::new(&mut store, &repo, &provider, MODEL, &s.worktrees)
        .keep_going(task, "keep going")
        .expect("keep going");
    assert!(matches!(held, TaskState::Holding { .. }), "{held:?}");
    let kept = body.len();

    let mut second = Typist {
        says: VecDeque::from([Said::Ask("fix the test".into()), Said::Quit]),
        ..Typist::default()
    };
    chat(
        &s,
        &mut store,
        task,
        Cause::Retry { of: landed.attempt },
        &mut body,
        &provider,
        &mut second,
        FAILING,
    );

    let refusal = &body.messages()[kept];
    assert_eq!(refusal.role, Role::User);
    assert!(
        refusal.content.contains("A check has already refused"),
        "{}",
        refusal.content
    );
    assert!(
        second.notes.iter().any(|n| n.contains("refused")),
        "{:?}",
        second.notes
    );
}

/// Stopping a turn stops that turn; the chat is still there for the next line.
#[test]
fn an_interrupted_turn_does_not_end_the_chat() {
    let s = subject();
    let mut store = Store::in_memory().expect("store");
    let task = seed(&mut store);
    let provider = Scripted::new(vec![Script::says("never reached")]);
    let mut operator = Typist {
        says: VecDeque::from([Said::Quit]),
        interrupt: true,
        ..Typist::default()
    };
    let mut body = Body::new();

    let landed = chat(
        &s,
        &mut store,
        task,
        Cause::Fresh,
        &mut body,
        &provider,
        &mut operator,
        PASSING,
    );

    assert_eq!(operator.turns, vec!["stopped"]);
    assert!(
        matches!(landed.state, TaskState::Holding { .. }),
        "{:?}",
        landed.state
    );
    assert!(events(&store).iter().any(|e| matches!(
        e,
        Event::Note { text } if text.contains("the operator stopped the turn")
    )));
}
