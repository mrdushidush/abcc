//! The tool layer's maintenance contract, driven through the real policy.
//!
//! ADR-0014 §2 asks for the donor's egress-registry shape copied exactly: one
//! const, a maintenance contract, and **an integration test that drives every
//! entry through the real dispatcher and asserts the refusal**. That is this
//! file. It is not a unit test of a matcher — every assertion below goes through
//! [`Policy::admits`], the same call the turn loop makes.

use abcc_engine::tools::{Destructive, Reach, TOOLS, ToolSpec, destructive_git, lookup};
use abcc_engine::{Head, Policy, Tier};

/// Every ceiling a role can actually be given, bottom to top.
const CEILINGS: [Tier; 4] = [Tier::NoTools, Tier::Read, Tier::Write, Tier::Exec];

fn exec_class() -> Vec<&'static ToolSpec> {
    TOOLS.iter().filter(|t| t.in_exec_class()).collect()
}

// ---------------------------------------------------------------------------
// The class
// ---------------------------------------------------------------------------

/// 🚨 The maintenance contract itself: **a tool that starts a child process is
/// in the exec class**, and the class is a property the entry declares rather
/// than a second column that can drift from it.
#[test]
fn a_tool_that_spawns_a_child_is_exactly_a_tool_at_exec() {
    for t in TOOLS {
        assert_eq!(
            t.in_exec_class(),
            t.required_tier() == Tier::Exec,
            "{} declares reach {:?} and requires {}",
            t.name,
            t.reach,
            t.required_tier()
        );
    }
    assert!(
        !exec_class().is_empty(),
        "an empty exec class would make every assertion below vacuous"
    );
}

/// **This is the test that fails when a new tool joins the class and is not
/// gated.** It drives every entry through the real policy at every ceiling.
#[test]
fn the_exec_class_is_refused_at_every_ceiling_below_exec() {
    for ceiling in CEILINGS {
        let policy = Policy::new("test-role", ceiling);
        for t in TOOLS {
            let admitted = policy.admits(t.name).is_ok();
            let should = t.required_tier() <= ceiling;
            assert_eq!(
                admitted,
                should,
                "{} (needs {}) at ceiling {ceiling}",
                t.name,
                t.required_tier()
            );
            if t.in_exec_class() && ceiling != Tier::Exec {
                assert!(!admitted, "{} was admitted at ceiling {ceiling}", t.name);
            }
        }
    }
}

/// 🚨 The donor defect this whole design exists for (F404–F406): `write_file` is
/// path-sandboxed and `bash` is not, so **`write_file` on a new path plus
/// `run_tests` is arbitrary code execution at the tier that never prompts**.
/// Here the pair cannot be assembled below `Exec`, because the second half of it
/// is in the denied class.
#[test]
fn write_file_and_run_tests_cannot_both_be_had_below_exec() {
    let writer = Policy::new("test-role", Tier::Write);
    assert!(writer.admits("write_file").is_ok());
    let denied = writer
        .admits("run_tests")
        .expect_err("run_tests spawns a child");
    assert!(
        denied.to_string().contains("capped at write"),
        "the refusal must name the ceiling: {denied}"
    );
}

/// A model that invents a tool is refused down the same path as one that reaches
/// above its ceiling. Two refusal shapes, one code path.
#[test]
fn an_invented_tool_is_refused_by_the_same_call() {
    let policy = Policy::new("test-role", Tier::Exec);
    let denied = policy
        .admits("exfiltrate")
        .expect_err("there is no such tool");
    assert!(denied.to_string().contains("exfiltrate"), "{denied}");
    assert!(lookup("exfiltrate").is_none());
}

/// `lookup` returns the first match, so a duplicated name would silently shadow
/// an entry — including shadowing an exec-class entry with a read-only one.
#[test]
fn the_registry_has_no_duplicate_names() {
    let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate tool name in TOOLS");
}

// ---------------------------------------------------------------------------
// The roles
// ---------------------------------------------------------------------------

/// ADR-0014 §1: the Planner and the Judge are capped below the exec class, and
/// the Judge below every class — A4 is one call with no tools, so a phase that
/// cannot call a tool cannot be talked into calling one.
#[test]
fn only_builders_reaches_the_exec_class() {
    for head in Head::ALL {
        let admits_exec = exec_class()
            .iter()
            .any(|t| head.policy().admits(t.name).is_ok());
        assert_eq!(
            admits_exec,
            head == Head::Builders,
            "{head} admits the exec class"
        );
    }
    assert!(Head::Commandos.tools().is_empty());
    assert!(
        Head::Recon.policy().admits("write_file").is_err(),
        "Recon is read-only and must not be able to edit"
    );
}

/// The advertised surface and the enforced surface are one list, not two that
/// agree today.
#[test]
fn a_head_admits_exactly_what_it_advertises() {
    for head in Head::ALL {
        for t in head.tools() {
            assert!(
                head.policy().admits(t.name).is_ok(),
                "{head} advertises {} and refuses it",
                t.name
            );
        }
        let advertised: Vec<&str> = head.tools().iter().map(|t| t.name).collect();
        for t in TOOLS {
            if !advertised.contains(&t.name) {
                assert!(
                    head.policy().admits(t.name).is_err(),
                    "{head} admits {} without advertising it",
                    t.name
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The backstop
// ---------------------------------------------------------------------------

/// All seven operations, through the git tool's own argument shape.
#[test]
fn the_table_recognises_all_seven_operations() {
    let cases = [
        ("git reset --hard HEAD~1", Destructive::ResetHard),
        ("git checkout -f main", Destructive::CheckoutForce),
        ("git switch --force main", Destructive::SwitchForce),
        ("git push --force origin main", Destructive::PushForce),
        ("git branch -D feature", Destructive::BranchForceDelete),
        ("git clean -fd", Destructive::CleanForce),
        ("git stash drop", Destructive::StashDrop),
    ];
    for (line, expected) in cases {
        assert_eq!(destructive_git(line), Some(expected), "{line}");
    }
}

/// 🚨 The two holes in the donor's guard, closed. Its careful copy keys on
/// `cmd_word != "git"` and so returns nothing for a shell wrapper (F422) —
/// which is the hole that matters, because the shell is what a model reaches for
/// when the git tool refuses. And `git clean -fd` and `git stash drop` were
/// caught by neither copy.
#[test]
fn a_shell_wrapper_does_not_launder_the_command() {
    for line in [
        r#"sh -c "git reset --hard""#,
        "bash -lc 'git clean -fd'",
        "pwsh -Command git push --force",
        "/usr/bin/git branch -D main",
        "C:\\Program Files\\Git\\cmd\\git.exe reset --hard",
        "cd sub && git stash drop",
    ] {
        assert!(
            destructive_git(line).is_some(),
            "the backstop missed: {line}"
        );
    }
}

/// A short cluster carries its letters: `-fd` and `-df` both mean `--force`
/// here. A long option is never a cluster, so `--dry-run` cannot trip on `f`.
#[test]
fn a_short_cluster_carries_its_letters_and_a_long_option_does_not() {
    assert_eq!(
        destructive_git("git clean -df"),
        Some(Destructive::CleanForce)
    );
    assert_eq!(
        destructive_git("git clean -xdf"),
        Some(Destructive::CleanForce)
    );
    assert_eq!(destructive_git("git clean --dry-run"), None);
    assert_eq!(destructive_git("git clean -n"), None);
}

/// The safe forms stay safe. `--force-with-lease` is not `--force`, and the
/// everyday commands are not refused.
#[test]
fn the_ordinary_commands_pass() {
    for line in [
        "git status",
        "git add -A",
        "git commit -m \"first\"",
        "git push origin main",
        "git push --force-with-lease origin main",
        "git branch -d merged",
        "git checkout -b feature",
        "git stash list",
        "git diff --name-status HEAD",
    ] {
        assert_eq!(destructive_git(line), None, "false refusal: {line}");
    }
}

/// ⚠ **What the backstop over-matches, stated rather than hidden.** A value
/// token after `-m` is skipped, so an ordinary message is safe — but a message
/// that itself contains the word `git` re-opens the scan, and the line is
/// refused. That is the direction a backstop should err in, and it is written
/// down here so nobody discovers it as a surprise.
#[test]
fn the_over_match_is_deliberate_and_bounded() {
    assert_eq!(
        destructive_git("git commit -m \"reset --hard the docs\""),
        None,
        "a flag value must not be scanned as arguments"
    );
    assert_eq!(
        destructive_git("git commit -m \"explain git reset --hard\""),
        Some(Destructive::ResetHard),
        "a second `git` inside a message re-opens the scan; this is the over-match"
    );
}

/// The backstop is not the control, and the class check does not consult it —
/// they are two mechanisms and the refusal shapes are distinct.
#[test]
fn the_backstop_is_separate_from_the_ceiling() {
    let recon = Head::Recon.policy();
    // Recon cannot run git at all: the class check refuses before any argument
    // is looked at.
    assert!(recon.admits("git").is_err());
    // Builders can, and the argument check is what looks at the line.
    assert!(Head::Builders.policy().admits("git").is_ok());
    assert_eq!(
        lookup("git").map(|t| t.reach),
        Some(Reach::SpawnsChild),
        "git spawns a child, so it is in the class regardless of its arguments"
    );
}
