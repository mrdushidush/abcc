//! `abcc land`, against a real repository and a real log.
//!
//! 🚨 **The claim these exist for is W13's M1** — *the pipeline runs end to end
//! with the human's only act being merge*. The unit tests beside the module
//! decide *which* attempt is entitled to land; these decide whether the work
//! actually arrives on the branch, which is the half that cannot be tested with
//! a fixture because it is git's answer and not ours.
//!
//! The shape each test builds is the one the driver builds: a checkpoint before,
//! a change, a checkpoint after, rungs measured at the second — and then the
//! operator's checkout put back, because **the whole point is that the change is
//! somewhere the operator's tree is not.**

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;

use abcc::Home;
use abcc::cli::{Command, Invocation, TaskRef};
use abcc_core::event::Event;
use abcc_core::outcome::{Measurement, Outcome};
use abcc_core::seq::{AttemptId, CheckpointId, Seq, TaskId, UnitId};
use abcc_core::task::Command as Lifecycle;
use abcc_store::Store;
use abcc_vcs::{Repo, Sha, checkpoint_ref};

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

fn git(cwd: &Path, args: &[&str]) -> String {
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

fn subject() -> Subject {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("subject");
    fs::create_dir_all(&root).expect("mkdir");
    git(dir.path(), &["init", "-q", "-b", "main", "subject"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "test"]);
    // ⚠ Pinned, because the machine's setting is not the fixture's: GitHub's
    // Windows image sets `core.autocrlf=true` system-wide, so every file git
    // writes back here comes out CRLF and the byte comparisons below fail on
    // git's conversion rather than on anything `land` did.
    git(&root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("src.rs"), "pub fn one() -> u32 { 1 }\n").expect("write");
    fs::write(root.join("other.rs"), "pub fn two() -> u32 { 2 }\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "first"]);
    Subject {
        home: dir.path().join("state"),
        _dir: dir,
        root,
    }
}

impl Subject {
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

    fn repo(&self) -> Repo {
        Repo::open(&self.root).expect("repo")
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

/// A task that ran once, changed `src.rs`, and whose gate went green.
///
/// It returns with the operator's checkout **back where it started**, which is
/// the state a real landing finds: the attempt worked in a worktree and the
/// change exists only as a pair of checkpoints.
fn a_green_task(subject: &Subject, change: &str, rungs: &[(&str, i32)]) -> (TaskId, Sha) {
    subject
        .run(Command::Task {
            prompt: "make one() return two".to_owned(),
            title: Some("one() returns two".to_owned()),
        })
        .expect("task");
    let task = subject
        .store()
        .tasks()
        .expect("tasks")
        .last()
        .expect("one task")
        .id;

    let repo = subject.repo();
    let mut store = subject.store();

    let before = repo
        .checkpoint(&checkpoint_ref("m1", 1), "before")
        .expect("before");
    let opened = store
        .append(Event::CheckpointTaken {
            task,
            sha: before.as_str().to_owned(),
            git_ref: checkpoint_ref("m1", 1),
        })
        .expect("checkpoint event");

    store
        .apply(task, Lifecycle::Deploy { unit: UnitId(0) })
        .expect("deploy");
    let started = store
        .append(Event::AttemptStarted {
            task,
            unit: UnitId(0),
            cause: abcc_core::attempt::Cause::Fresh,
            checkpoint_from: Some(CheckpointId::at(opened.seq)),
        })
        .expect("attempt");
    let attempt = AttemptId::at(started.seq);
    store
        .apply(task, Lifecycle::Engage { attempt })
        .expect("engage");

    // The work. In a real attempt this happens in a worktree; here it happens in
    // the checkout and is taken back out below, which reaches the same place.
    fs::write(subject.root.join("src.rs"), change).expect("write");
    let after = repo
        .checkpoint(&checkpoint_ref("m1", 2), "after")
        .expect("after");
    store
        .append(Event::CheckpointTaken {
            task,
            sha: after.as_str().to_owned(),
            git_ref: checkpoint_ref("m1", 2),
        })
        .expect("checkpoint event");

    for (rung, exit) in rungs {
        store
            .append(Event::RungRecorded {
                attempt,
                outcome: Outcome::Measured(Measurement {
                    rung: (*rung).to_owned(),
                    sha: after.as_str().to_owned(),
                    exit: *exit,
                    counts: None,
                    detail: String::new(),
                }),
            })
            .expect("rung");
    }

    // 🚨 The operator's checkout goes back to where it was. Without this the
    // test would land a change the tree already holds and the patch would be a
    // no-op that still passed.
    repo.restore(&before).expect("restore");
    (task, after)
}

const TWO: &str = "pub fn one() -> u32 { 2 }\n";

// ---------------------------------------------------------------------------

/// 🚨 **M1, end to end.** The change was never in the operator's tree, and after
/// the verb it is on the branch, in a commit, with the log saying so.
#[test]
fn a_green_attempt_becomes_a_commit_on_the_branch() {
    let subject = subject();
    let (task, after) = a_green_task(&subject, TWO, &[("structural", 0), ("acceptance", 0)]);

    let before_head = subject.repo().head().expect("head");
    let said = subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect("land");

    // The work is in the tree...
    assert_eq!(
        fs::read_to_string(subject.root.join("src.rs")).expect("read"),
        TWO
    );
    // ...and in a commit, not merely staged.
    let head = subject.repo().head().expect("head");
    assert_ne!(head, before_head, "the branch did not move");
    assert!(
        subject.repo().is_clean().expect("clean"),
        "a landing left the tree dirty: {}",
        git(&subject.root, &["status", "--porcelain"])
    );
    // The commit is the one the gate measured, and nothing else came with it.
    let touched = git(&subject.root, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(touched.split_whitespace().collect::<Vec<_>>(), ["src.rs"]);

    // 🚨 F433: after a squash the trailer is the only record of authorship.
    let body = git(&subject.root, &["log", "-1", "--format=%B"]);
    assert!(body.contains("Co-authored-by: abcc"), "{body}");
    assert!(body.starts_with("one() returns two"), "{body}");
    assert!(body.contains("2 rung(s) green"), "{body}");

    // 🚨 The line that makes the next `abcc review` one command — the reason a
    // landing names a sha at all.
    assert!(said.contains(head.short()), "{said}");
    assert!(said.contains("abcc review"), "{said}");
    assert!(said.contains("2 rung(s) green"), "{said}");

    // And the log carries the act.
    assert!(
        subject.kinds().contains(&"change_landed"),
        "{:?}",
        subject.kinds()
    );
    let landed = subject
        .store()
        .read_from(Seq::ORIGIN, 1000)
        .expect("read")
        .into_iter()
        .find_map(|l| match l.event {
            Event::ChangeLanded {
                change, to, rungs, ..
            } => Some((change, to, rungs)),
            _ => None,
        })
        .expect("a change_landed row");
    assert_eq!(landed.0, head.as_str());
    assert_eq!(landed.1, after.as_str());
    assert_eq!(landed.2, 2);
}

/// An old pair still lands after the branch has moved on: the change arrives and
/// the commits that happened meanwhile are still there, because a landing
/// applies one diff rather than checking out a tree.
///
/// ⚠ **What this does NOT prove is that `--3way` is doing the work.** The
/// meanwhile commits are in a file the attempt never touched, so the direct
/// applier handles it too and the test would pass with `--3way` deleted. The
/// evidence for the fallback is in `Repo::apply`'s doc comment, measured on this
/// repository's own five green pairs at 16–58 commits behind `main`; keeping
/// that claim here as well would be asserting it twice and testing it neither
/// time.
#[test]
fn a_pair_whose_parent_is_behind_the_branch_still_lands() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0)]);

    for n in 0..3 {
        fs::write(
            subject.root.join("other.rs"),
            format!("pub fn two() -> u32 {{ {} }}\n", n + 10),
        )
        .expect("write");
        git(
            &subject.root,
            &["commit", "-aqm", &format!("meanwhile {n}")],
        );
    }

    subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect("land");
    assert_eq!(
        fs::read_to_string(subject.root.join("src.rs")).expect("read"),
        TWO
    );
    assert!(
        fs::read_to_string(subject.root.join("other.rs"))
            .expect("read")
            .contains("12"),
        "the landing clobbered the meanwhile commits"
    );
}

/// 🚨 **`git apply --3way` is not all-or-nothing, and this is the test that
/// found it.** The three-way fallback is a merge, so a hunk it cannot reconcile
/// is written into the file with conflict markers and left unmerged in the index
/// — `Applied patch to 'src.rs' with conflicts. U src.rs`, exit 1. The first
/// version of the verb returned that error and stopped, which handed the
/// operator a checkout in mid-merge for a landing that never happened.
///
/// So the assertion that matters is not the refusal, which was always there. It
/// is **the tree afterwards.**
#[test]
fn a_conflicting_pair_refuses_and_leaves_the_checkout_clean() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0)]);

    // Somebody else rewrote the same one-line file the attempt changed, so there
    // is no reconciling the two: both edits own the whole file.
    fs::write(
        subject.root.join("src.rs"),
        "//! a header that arrived later\n\npub fn one() -> u32 { 7 }\n",
    )
    .expect("write");
    git(&subject.root, &["commit", "-aqm", "meanwhile"]);
    let head = subject.repo().head().expect("head");

    let err = subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect_err("refused");
    assert!(format!("{err}").contains("conflict"), "{err}");

    // 🚨 The tree is exactly where it was: no markers, no unmerged index entry,
    // no moved branch, and nothing on the log claiming a landing.
    let after = fs::read_to_string(subject.root.join("src.rs")).expect("read");
    assert!(
        !after.contains("<<<<<<<"),
        "conflict markers left behind:\n{after}"
    );
    assert!(after.contains("pub fn one() -> u32 { 7 }"), "{after}");
    assert!(
        subject.repo().is_clean().expect("clean"),
        "the checkout was left dirty: {}",
        git(&subject.root, &["status", "--porcelain"])
    );
    assert_eq!(subject.repo().head().expect("head"), head);
    assert!(!subject.kinds().contains(&"change_landed"));
}

/// ⚠ Applying a patch over uncommitted work is how an operator loses it, so the
/// verb refuses before it touches anything.
#[test]
fn a_dirty_checkout_refuses_before_anything_is_applied() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0)]);
    fs::write(
        subject.root.join("other.rs"),
        "pub fn two() -> u32 { 99 }\n",
    )
    .expect("write");

    let err = subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect_err("refused");
    let said = format!("{err}");
    assert!(said.contains("uncommitted"), "{said}");
    // The refusal cost nothing: the operator's edit is untouched and the
    // attempt's change did not arrive.
    assert!(
        fs::read_to_string(subject.root.join("other.rs"))
            .expect("read")
            .contains("99")
    );
    assert_eq!(
        fs::read_to_string(subject.root.join("src.rs")).expect("read"),
        "pub fn one() -> u32 { 1 }\n"
    );
    assert!(!subject.kinds().contains(&"change_landed"));
}

/// The ladder's unit is the change, so one piece of work is one commit and one
/// row. The second landing is refused by the log rather than by git.
#[test]
fn landing_twice_is_refused() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0)]);
    subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect("land");
    let err = subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect_err("refused");
    assert!(format!("{err}").contains("already landed"), "{err}");
    assert_eq!(
        subject
            .kinds()
            .iter()
            .filter(|k| **k == "change_landed")
            .count(),
        1
    );
}

/// 🚨 One red rung is the whole gate red. The tree compiles, the work is real,
/// and it is not entitled to be on the branch — which is the rule M2 is built
/// on and the one this project's own `--version` arms keep meeting.
#[test]
fn a_red_rung_keeps_the_work_off_the_branch() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0), ("standard", 101)]);
    let head = subject.repo().head().expect("head");

    let err = subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect_err("refused");
    assert!(format!("{err}").contains("entitled to land"), "{err}");
    assert_eq!(subject.repo().head().expect("head"), head);
    assert_eq!(
        fs::read_to_string(subject.root.join("src.rs")).expect("read"),
        "pub fn one() -> u32 { 1 }\n"
    );
}

/// 🚨 **The denominator F727 says the ladder does not have** — in the one
/// direction it is sound to state it. A landing is a row; a review of it either
/// is or is not.
#[test]
fn a_landing_is_unreviewed_until_somebody_reviews_it() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0)]);
    subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect("land");

    let log = abcc::fun::read_all(&subject.store()).expect("read");
    let ladder = &abcc_core::replay::Replay::over(&log).ladder;
    assert_eq!(ladder.landings.len(), 1);
    assert_eq!(ladder.unreviewed().len(), 1, "nobody has reviewed it yet");

    let change = ladder.landings[0].change.clone();
    subject
        .run(Command::Review {
            change: change.clone(),
            seconds: 600,
            by: Some("david".to_owned()),
            crossed_boundary: true,
        })
        .expect("review");

    let log = abcc::fun::read_all(&subject.store()).expect("read");
    let ladder = &abcc_core::replay::Replay::over(&log).ladder;
    assert_eq!(ladder.landings.len(), 1);
    assert!(
        ladder.unreviewed().is_empty(),
        "the review did not find its landing"
    );
    assert_eq!(ladder.crossed(), 1);
    assert_eq!(ladder.median_seconds(), Some(600));
}

// ---------------------------------------------------------------------------
// abcc diff — the reading this module's own doc said was missing
// ---------------------------------------------------------------------------

/// 🚨 **The property the verb exists for: what you read is what lands.**
///
/// Not *a diff of the same change* — the same bytes. `diff` and `land` take
/// their pair from one function, and this asserts the patch the operator was
/// shown is the patch git committed. A review is worth nothing if the thing
/// reviewed and the thing merged are two derivations of one change, because
/// then the minutes were spent on something that is not what shipped.
#[test]
fn what_diff_prints_is_what_land_commits() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0), ("acceptance", 0)]);

    let shown = subject
        .run(Command::Diff {
            task: TaskRef(task.born().get()),
        })
        .expect("diff");
    assert!(shown.contains("would apply"), "{shown}");
    assert!(shown.contains("pub fn one() -> u32 { 2 }"), "{shown}");

    subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect("land");
    let committed = git(&subject.root, &["show", "HEAD", "--format=", "--unified=3"]);

    let body = |patch: &str| {
        patch
            .lines()
            .filter(|l| {
                (l.starts_with('+') || l.starts_with('-'))
                    && !l.starts_with("+++")
                    && !l.starts_with("---")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        !body(&shown).is_empty(),
        "the diff printed no hunks:\n{shown}"
    );
    assert_eq!(
        body(&shown),
        body(&committed),
        "shown:\n{shown}\ncommitted:\n{committed}"
    );
}

/// 🚨 **A red attempt is the case with the most to show, and the one `replay`
/// cannot reach.** It says which rung stopped the attempt; it has never been
/// able to say what the attempt had written when it stopped. 88 of 125 attempts
/// on this project's own log never reached a gradeable artifact.
#[test]
fn a_red_attempt_still_shows_what_it_wrote() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0), ("standard", 101)]);

    subject
        .run(Command::Land {
            task: TaskRef(task.born().get()),
        })
        .expect_err("a red gate does not land");

    let shown = subject
        .run(Command::Diff {
            task: TaskRef(task.born().get()),
        })
        .expect("diff reads a red attempt");
    assert!(shown.contains("will not land"), "{shown}");
    assert!(
        shown.contains("pub fn one() -> u32 { 2 }"),
        "the work is unreadable exactly when it matters:\n{shown}"
    );
}

/// ⚠ **Read-only, and the fleet may call it.** Every neighbour in this module
/// moves a state or appends a row; this one is a window. If it ever writes, it
/// stops being safe to run mid-flight, which is when it is most useful.
#[test]
fn a_diff_writes_no_row_and_moves_no_state() {
    let subject = subject();
    let (task, _) = a_green_task(&subject, TWO, &[("structural", 0)]);
    let before = subject.kinds();
    let head = subject.repo().head().expect("head");

    subject
        .run(Command::Diff {
            task: TaskRef(task.born().get()),
        })
        .expect("diff");

    assert_eq!(subject.kinds(), before, "diff appended to the log");
    assert_eq!(
        subject.repo().head().expect("head"),
        head,
        "diff moved HEAD"
    );
}
