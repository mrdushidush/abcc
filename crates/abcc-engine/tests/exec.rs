//! The four tools that start a child, against real processes.
//!
//! These are the tools the whole permission argument is about: the class is
//! denied rather than argued with, because an argument check binds the tool that
//! has an argument and a shell walks past it. What is testable here is what the
//! host watched — and that the two things the tool layer *does* promise about a
//! child, its working directory and its honesty about how it ended, are true.

use std::fs;
use std::time::Duration;

use abcc_core::outcome::Why;
use abcc_engine::provider::ToolCall;
use abcc_engine::tools::{Confinement, lookup};
use abcc_engine::turn::{ToolResult, Tools};
use abcc_engine::workspace::{TOOLCHAINS, Toolchain, Workspace};

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
};

#[cfg(not(windows))]
const ECHOING: Toolchain = Toolchain {
    name: "echoing",
    witnesses: &[],
    test: &["sh", "-c", "echo ran the tests; exit 3"],
    diagnostics: &["sh", "-c", "echo checked"],
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
    }
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
