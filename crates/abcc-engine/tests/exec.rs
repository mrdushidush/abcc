//! The four tools that start a child, against real processes.
//!
//! These are the tools the whole permission argument is about: the class is
//! denied rather than argued with, because an argument check binds the tool that
//! has an argument and a shell walks past it. What is testable here is what the
//! host watched — and that the two things the tool layer *does* promise about a
//! child, its working directory and its honesty about how it ended, are true.

use std::fs;
use std::time::Duration;

use abcc_core::outcome::{Reading, Why};
use abcc_engine::provider::ToolCall;
use abcc_engine::tools::{Confinement, lookup};
use abcc_engine::turn::{ToolResult, Tools};
use abcc_engine::workspace::{Standard, TOOLCHAINS, Toolchain, Workspace};

fn run(workspace: &Workspace, tool: &str, arguments: &serde_json::Value) -> ToolResult {
    let spec = lookup(tool).expect("registry");
    workspace.run(
        spec,
        &ToolCall {
            id: "call-1".to_owned(),
            tool: tool.to_owned(),
            arguments: arguments.to_string(),
        },
    )
}

/// A profile that needs no toolchain installed, so the wiring is what is under
/// test rather than cargo.
///
/// ⚠ It names the platform's own interpreter rather than `bash`, because a
/// toolchain profile spawns the program it names and on this platform the name
/// `bash` reaches the WSL relay (F492). The `bash` *tool* resolves its shell; a
/// profile does not, and a profile is operator configuration.
#[cfg(windows)]
const ECHOING: Toolchain = Toolchain {
    name: "echoing",
    witnesses: &[],
    test: &["cmd", "/C", "echo ran the tests & exit 3"],
    diagnostics: &["cmd", "/C", "echo checked"],
    // Exit 3 is "interrupted before running anything" to a python profile and
    // "no `test result:` line" to a cargo one; both read it as an absence, and
    // this fixture is about the tool layer's wiring rather than the reading.
    reading: Reading::Python,
    standard: None,
};

#[cfg(not(windows))]
const ECHOING: Toolchain = Toolchain {
    name: "echoing",
    witnesses: &[],
    test: &["sh", "-c", "echo ran the tests; exit 3"],
    diagnostics: &["sh", "-c", "echo checked"],
    reading: Reading::Python,
    standard: None,
};

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

/// 🚨 F492. The first `bash` on PATH here is the WSL relay, which answers
/// `execvpe(/bin/bash) failed` **at exit 1** — so a tool layer that spawns the
/// name records *the command failed* about a shell that never ran. This test is
/// the one that would notice: it asserts a working shell, in the workspace, with
/// output.
#[test]
fn bash_runs_a_command_in_the_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("marker-4a1c.txt"), "here").expect("write");
    let workspace = Workspace::open(dir.path()).expect("open");

    let result = run(
        &workspace,
        "bash",
        &serde_json::json!({ "command": "ls -1" }),
    );

    assert_eq!(
        result.unmeasured, None,
        "no working shell was found: {}",
        result.text
    );
    assert_eq!(result.exit, Some(0), "{}", result.text);
    assert!(
        result.text.contains("marker-4a1c.txt"),
        "the shell listed a different directory: {}",
        result.text
    );
}

/// A tool that fails is measured, not unmeasured: there is an exit status and it
/// is not zero. The two are different answers and the report type has room for
/// both.
#[test]
fn a_failing_command_has_an_exit_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");

    let result = run(
        &workspace,
        "bash",
        &serde_json::json!({ "command": "echo to stderr >&2; exit 7" }),
    );
    assert_eq!(result.exit, Some(7), "{}", result.text);
    assert_eq!(result.unmeasured, None);
    assert!(result.text.contains("exit 7"), "{}", result.text);
    assert!(result.text.contains("--- stderr"), "{}", result.text);
    assert!(result.text.contains("to stderr"), "{}", result.text);
}

/// ⚠ A timeout folded into "clean" is the worst available lie, because the
/// process may still be alive (F220). The budget the model asks for is honoured
/// and what it bought is an absence, not an exit code.
#[test]
fn a_command_that_outstays_the_budget_the_model_set_is_unmeasured() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");

    let result = run(
        &workspace,
        "bash",
        &serde_json::json!({ "command": "sleep 30", "timeout_ms": 400 }),
    );
    assert_eq!(result.exit, None, "{}", result.text);
    match result.unmeasured {
        Some(Why::Timeout { after_ms }) => assert!(after_ms >= 400, "{after_ms} ms"),
        other => panic!("expected a timeout, got {other:?}: {}", result.text),
    }
}

// ---------------------------------------------------------------------------
// The backstop
// ---------------------------------------------------------------------------

/// 🚨 F422: the donor's guard keys on the first word being git, so
/// `sh -c "git reset --hard"` returns nothing from it. The table here is read
/// from the whole line, through both tools, so a wrapper does not launder the
/// command. ⚠ It is a backstop and never the control — the boundary is the
/// worktree.
#[test]
fn a_destructive_git_command_is_refused_through_both_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");

    let through_git = run(
        &workspace,
        "git",
        &serde_json::json!({ "args": ["reset", "--hard", "HEAD~1"] }),
    );
    assert!(
        through_git.text.contains("git reset --hard"),
        "{}",
        through_git.text
    );

    let through_shell = run(
        &workspace,
        "bash",
        &serde_json::json!({ "command": "cd /tmp && git push --force origin main" }),
    );
    assert!(
        through_shell.text.contains("git push --force"),
        "{}",
        through_shell.text
    );

    for refused in [&through_git, &through_shell] {
        match &refused.unmeasured {
            Some(Why::FailedBeforeRunning { detail }) => assert_eq!(&refused.text, detail),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

/// An ordinary git command is not refused, which is the half of the backstop
/// that a table with a hole in it would also pass. It is here so the test above
/// cannot be satisfied by refusing everything.
#[test]
fn an_ordinary_git_command_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");

    let result = run(
        &workspace,
        "git",
        &serde_json::json!({ "args": ["--version"] }),
    );
    assert_eq!(result.unmeasured, None, "{}", result.text);
    assert_eq!(result.exit, Some(0), "{}", result.text);
    assert!(result.text.contains("git version"), "{}", result.text);
}

// ---------------------------------------------------------------------------
// The toolchain profile
// ---------------------------------------------------------------------------

/// ⚠ **No profile is an absence, not a failure.** There is no checker for this
/// workspace and saying so is the honest answer; a zero would not be.
#[test]
fn a_workspace_with_no_toolchain_says_there_is_no_checker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");
    assert_eq!(workspace.toolchain(), None);

    let result = run(&workspace, "run_tests", &serde_json::json!({}));
    assert_eq!(result.exit, None);
    assert!(
        matches!(result.unmeasured, Some(Why::NoCheckerForArtifact { .. })),
        "{:?}",
        result.unmeasured
    );
    assert!(result.text.contains("Cargo.toml"), "{}", result.text);
}

#[test]
fn the_witness_file_is_what_detects_a_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write");
    let workspace = Workspace::open(dir.path()).expect("open");
    assert_eq!(workspace.toolchain().map(|t| t.name), Some("cargo"));

    // Every profile in the table names at least one witness and both commands,
    // because a profile that names none of them is a profile that cannot run.
    for profile in TOOLCHAINS {
        assert!(
            !profile.witnesses.is_empty(),
            "{} has no witness",
            profile.name
        );
        assert!(!profile.test.is_empty(), "{} cannot test", profile.name);
        assert!(
            !profile.diagnostics.is_empty(),
            "{} cannot check",
            profile.name
        );
        // A declared standard with no commands is a rung that cannot refuse
        // anything, which is worse than an absent one: it reports green.
        if let Some(standard) = profile.standard {
            assert!(
                !standard.commands.is_empty(),
                "{} declares a standard with no commands",
                profile.name
            );
            assert!(
                standard.commands.iter().all(|c| !c.is_empty()),
                "{} declares an empty command",
                profile.name
            );
        }
    }
}

/// 🚨 **F555, pinned: rustfmt is in the cargo standard, and it runs first.**
///
/// The finding was a run reaching MISSION ACCOMPLISHED on a tree
/// `cargo fmt --check` refuses — four green rungs, a Judge with no findings, and
/// rustfmt in none of the rungs. David ruled on 2026-08-30 that the check is
/// added to the cargo standard unconditionally, so this asserts the shape of the
/// table rather than trusting a comment to hold it.
///
/// ⚠ `--color=never` is asserted too, and it is measured rather than tidy
/// (F557): rustfmt colours its diff even when stdout is a plain file rather than
/// a terminal, and this rung's output is written to a durable SQLite log and
/// re-rendered in the TUI.
#[test]
fn the_cargo_standard_checks_formatting_first_and_asks_for_no_colour() {
    let cargo = TOOLCHAINS
        .iter()
        .find(|t| t.name == "cargo")
        .expect("the cargo profile");
    let standard = cargo.standard.expect("cargo declares a standard");

    let first = standard.commands.first().expect("at least one command");
    assert_eq!(
        first,
        &["cargo", "fmt", "--check", "--", "--color=never"],
        "the formatting check is not the cargo standard's first command"
    );
    assert!(
        standard
            .commands
            .iter()
            .any(|c| c.contains(&"clippy") && c.contains(&"--all-targets")),
        "the cargo standard lost clippy"
    );
}

/// The runner passes the profile's command through and hands back what the host
/// watched — including a non-zero status, which it does not interpret. Reading a
/// runner's output into counts is the Gate's work, not this layer's.
#[test]
fn run_tests_runs_the_profiles_command_and_reports_what_happened() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path())
        .expect("open")
        .with_toolchain(ECHOING)
        .with_budget(Duration::from_secs(30));

    let result = run(&workspace, "run_tests", &serde_json::json!({}));
    assert_eq!(result.exit, Some(3), "{}", result.text);
    assert_eq!(result.unmeasured, None);
    assert!(result.text.contains("ran the tests"), "{}", result.text);
}

// ---------------------------------------------------------------------------
// diagnostics, and the standard the repository declares
// ---------------------------------------------------------------------------

/// A profile whose standard is two commands and whose witness is a file each
/// test decides whether to put in the tree. ⚠ Nothing here needs cargo
/// installed: what is under test is which commands the tool layer runs, and in
/// what order.
#[cfg(windows)]
const DECLARING: Toolchain = Toolchain {
    name: "declaring",
    witnesses: &[],
    test: &["cmd", "/C", "echo ran the tests"],
    diagnostics: &["cmd", "/C", "echo compiled"],
    reading: Reading::Cargo,
    standard: Some(Standard {
        witnesses: &["standard.toml"],
        commands: &[
            &["cmd", "/C", "echo formatted"],
            &["cmd", "/C", "echo linted"],
        ],
    }),
};

#[cfg(not(windows))]
const DECLARING: Toolchain = Toolchain {
    name: "declaring",
    witnesses: &[],
    test: &["sh", "-c", "echo ran the tests"],
    diagnostics: &["sh", "-c", "echo compiled"],
    reading: Reading::Cargo,
    standard: Some(Standard {
        witnesses: &["standard.toml"],
        commands: &[
            &["sh", "-c", "echo formatted"],
            &["sh", "-c", "echo linted"],
        ],
    }),
};

/// The same profile whose compiler refuses, so the conjunction has something to
/// stop at.
#[cfg(windows)]
const NOT_COMPILING: Toolchain = Toolchain {
    diagnostics: &["cmd", "/C", "echo E0004 non-exhaustive patterns & exit 101"],
    ..DECLARING
};

#[cfg(not(windows))]
const NOT_COMPILING: Toolchain = Toolchain {
    diagnostics: &["sh", "-c", "echo E0004 non-exhaustive patterns; exit 101"],
    ..DECLARING
};

/// 🚨🚨 **F673: the tool the model checks its own work with now runs the
/// commands the model is graded on.**
///
/// The defect was a criterion the model could not see. The standard rung is
/// `cargo fmt --check` then `cargo clippy -- -D warnings`, `diagnostics` was
/// `cargo check --all-targets`, and `cargo check` sees neither. Four attempts
/// reached that rung on the E0004 subject and **none passed**, while the rung is
/// passable on other subjects — so the fix is what the tool *measures* rather
/// than a sentence in the brief, which W7 measured at 39 of 50 for the best
/// prompt in its family and 0 of 50 for a better-written one.
#[test]
fn diagnostics_also_runs_the_standard_the_repository_declares() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("standard.toml"), "").expect("write");
    let workspace = Workspace::open(dir.path())
        .expect("open")
        .with_toolchain(DECLARING)
        .with_budget(Duration::from_secs(30));

    let result = run(&workspace, "diagnostics", &serde_json::json!({}));
    assert_eq!(result.exit, Some(0), "{}", result.text);
    assert_eq!(result.unmeasured, None);
    for ran in ["compiled", "formatted", "linted"] {
        assert!(
            result.text.contains(ran),
            "{ran} did not run: {}",
            result.text
        );
    }
    // The compiler first, and that is the one ordering question this had: the
    // standard's own order is cheapest-first, so a tree with a type error would
    // otherwise be handed a formatting refusal instead of its error.
    let at = |needle: &str| result.text.find(needle).expect(needle);
    assert!(at("compiled") < at("formatted"), "{}", result.text);
    assert!(at("formatted") < at("linted"), "{}", result.text);
}

/// 🚨 **A repository that declares no standard gets exactly what it got
/// before.**
///
/// [`Standard::declared_at`] is the whole of it, and it is the rule the gate
/// holds one level up: an undeclared rung is *absent* rather than missing, and
/// running a linter over a project that never opted into one is this tool having
/// an opinion about somebody else's code. The witness is a file in the tree, so
/// a profile can carry a standard the repository has not asked for.
#[test]
fn diagnostics_stays_the_compiler_where_no_standard_is_declared() {
    let dir = tempfile::tempdir().expect("tempdir");
    // `DECLARING` names `standard.toml` as its witness, and this tree has none.
    let workspace = Workspace::open(dir.path())
        .expect("open")
        .with_toolchain(DECLARING)
        .with_budget(Duration::from_secs(30));

    let result = run(&workspace, "diagnostics", &serde_json::json!({}));
    assert_eq!(result.exit, Some(0), "{}", result.text);
    assert!(result.text.contains("compiled"), "{}", result.text);
    assert!(
        !result.text.contains("formatted") && !result.text.contains("linted"),
        "a standard nobody declared was run anyway: {}",
        result.text
    );
}

/// ⚠ **A conjunction, and the first refusal ends it** — the same rule the
/// standard rung itself holds. A model that cannot compile is not owed a
/// formatting diff, and a red that names the wrong program is F555 read
/// backwards.
#[test]
fn diagnostics_stops_at_the_first_refusal() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("standard.toml"), "").expect("write");
    let workspace = Workspace::open(dir.path())
        .expect("open")
        .with_toolchain(NOT_COMPILING)
        .with_budget(Duration::from_secs(30));

    let result = run(&workspace, "diagnostics", &serde_json::json!({}));
    assert_eq!(result.exit, Some(101), "{}", result.text);
    assert!(result.text.contains("E0004"), "{}", result.text);
    assert!(
        !result.text.contains("formatted"),
        "the conjunction walked past a refusal: {}",
        result.text
    );
}

/// ⚠ **The standard is not the test suite's business.** `run_tests` runs the
/// profile's test command and nothing else, however the repository is declared:
/// two tools that ran the same lint would be two places for one red to come
/// from.
#[test]
fn run_tests_does_not_run_the_standard() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("standard.toml"), "").expect("write");
    let workspace = Workspace::open(dir.path())
        .expect("open")
        .with_toolchain(DECLARING)
        .with_budget(Duration::from_secs(30));

    let result = run(&workspace, "run_tests", &serde_json::json!({}));
    assert!(result.text.contains("ran the tests"), "{}", result.text);
    assert!(
        !result.text.contains("linted"),
        "run_tests ran the standard: {}",
        result.text
    );
}

/// 🚨 **The summary a model reads has to say what the tool measures**,
/// because this is the instrument it checks its own work with and F673 is what
/// happens when it understates. The tool list is in the frozen head (F81), so
/// this sentence is as durable as the code beside it.
#[test]
fn the_diagnostics_summary_says_it_runs_the_standard() {
    let spec = lookup("diagnostics").expect("registry");
    assert!(
        spec.summary.contains("standard"),
        "the summary stopped naming the standard: {}",
        spec.summary
    );
}

// ---------------------------------------------------------------------------
// The posture
// ---------------------------------------------------------------------------

/// 🚨 ADR-0014 §3: the posture is a value the console shows, and on this platform
/// it is the worktree and nothing else. A workspace that claimed anything
/// stronger would be claiming rather than measuring.
#[test]
fn the_workspace_says_what_actually_confines_its_children() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");
    assert_eq!(workspace.confinement(), Confinement::Cwd);
}
