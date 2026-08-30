//! The tool layer's maintenance contract, driven through the real policy.
//!
//! ADR-0014 §2 asks for the donor's egress-registry shape copied exactly: one
//! const, a maintenance contract, and **an integration test that drives every
//! entry through the real dispatcher and asserts the refusal**. That is this
//! file. It is not a unit test of a matcher — every assertion below goes through
//! [`Policy::admits`], the same call the turn loop makes.

use abcc_engine::tools::{Destructive, Reach, TOOLS, ToolSpec, destructive_git, lookup};
use abcc_engine::{Head, Policy, Posting, Tier};

/// Every ceiling a role can actually be given, bottom to top.
const CEILINGS: [Tier; 4] = Tier::ALL;

/// A head in a slot that caps nothing, which is what the role asked for on its
/// own. ⚠ Spelled out at every call site below rather than hidden in a helper
/// named `policy`, because the whole point of removing `Head::policy()` is that
/// *which slot* is not a question a head can answer by itself.
fn uncapped(head: Head) -> Posting {
    head.posted(Tier::Exec)
}

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
            .any(|t| uncapped(head).policy().admits(t.name).is_ok());
        assert_eq!(
            admits_exec,
            head == Head::Builders,
            "{head} admits the exec class"
        );
    }
    assert!(uncapped(Head::Commandos).tools().is_empty());
    assert!(
        uncapped(Head::Recon).policy().admits("write_file").is_err(),
        "Recon is read-only and must not be able to edit"
    );
}

/// The advertised surface and the enforced surface are one list, not two that
/// agree today.
///
/// ⚠ It runs over every **posting**, not every head: a slot cap is precisely the
/// thing that could make the two lists diverge, so checking it only at the
/// role's own ceiling would check it exactly where it cannot fail.
#[test]
fn a_posting_admits_exactly_what_it_advertises() {
    for posting in Posting::ALL {
        for t in posting.tools() {
            assert!(
                posting.policy().admits(t.name).is_ok(),
                "{posting} advertises {} and refuses it",
                t.name
            );
        }
        let advertised: Vec<&str> = posting.tools().iter().map(|t| t.name).collect();
        for t in TOOLS {
            if !advertised.contains(&t.name) {
                assert!(
                    posting.policy().admits(t.name).is_err(),
                    "{posting} admits {} without advertising it",
                    t.name
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The slot's ceiling — ADR-0014 §4's other half
// ---------------------------------------------------------------------------

/// 🚨 **A slot can only ever take capability away.**
///
/// The effective ceiling is the narrower of the role's and the slot's, so no
/// slot setting anywhere can hand a role something its own ceiling refuses it.
/// This is the property that makes `--ceiling` safe to expose on the command
/// line at all.
#[test]
fn a_slot_narrows_a_role_and_can_never_widen_one() {
    for head in Head::ALL {
        for slot in Tier::ALL {
            let posting = head.posted(slot);
            assert!(
                posting.ceiling() <= head.max_tier(),
                "{posting} exceeds {head}'s own ceiling of {}",
                head.max_tier()
            );
            assert!(
                posting.ceiling() <= slot,
                "{posting} exceeds the slot's ceiling of {slot}"
            );
            assert_eq!(
                posting.ceiling(),
                head.max_tier().min(slot),
                "{head} in a {slot} slot"
            );
        }
    }
}

/// ⚠ [`Tier::narrower`] is a discriminant comparison so that it can be `const`,
/// and a discriminant comparison is only the same thing as the derived `Ord` for
/// as long as the variants stay in order. All sixteen pairs, so reordering the
/// enum fails here rather than silently widening a ceiling.
#[test]
fn narrower_agrees_with_ord_over_every_pair() {
    for a in Tier::ALL {
        for b in Tier::ALL {
            assert_eq!(a.narrower(b), a.min(b), "{a} and {b}");
            assert_eq!(a.narrower(b), b.narrower(a), "{a} and {b} out of order");
        }
    }
}

/// 🚨 **The falsifier the milestone asked for**: a slot capped at `read` takes
/// the exec class away from `Builders`, which is the only head that reaches it.
///
/// ⚠ And note what the refusal *is*. `Why::Denied` is a normal outcome inside a
/// phase — the model is told, in its own transcript, and the round continues —
/// not an ending. A capped slot narrows a role; it does not kill an attempt.
#[test]
fn a_read_only_slot_takes_the_exec_class_from_builders() {
    let capped = Head::Builders.posted(Tier::Read);
    assert_eq!(capped.ceiling(), Tier::Read);
    for t in exec_class() {
        let denied = capped
            .policy()
            .admits(t.name)
            .expect_err("the exec class is denied below exec");
        assert!(
            denied.to_string().contains("capped at read"),
            "the refusal must name the ceiling in force: {denied}"
        );
    }
    // The role is intact underneath: the cap is the slot's, not a mutation.
    assert!(
        uncapped(Head::Builders)
            .policy()
            .admits("run_tests")
            .is_ok(),
        "the same role in an uncapped slot still reaches exec"
    );
}

/// 🚨 **A cap narrows what the model is told, and that is the difference between
/// a policy and a trap.**
///
/// A capped `Builders` whose prefix still advertised `run_tests` would spend a
/// tool round per attempt learning something the prompt could have said. So the
/// prefix is composed from the *effective* ceiling, and this checks the half a
/// `Policy::admits` test can never see.
#[test]
fn a_capped_slot_stops_advertising_what_it_will_refuse() {
    let full = uncapped(Head::Builders).prefix();
    let capped = Head::Builders.posted(Tier::Read).prefix();
    assert_ne!(full, capped, "the cap did not reach the prompt");
    for t in exec_class() {
        let declaration = format!("\n{} — ", t.name);
        assert!(
            full.contains(&declaration),
            "an uncapped Builders should declare {}",
            t.name
        );
        assert!(
            !capped.contains(&declaration),
            "a read-capped Builders still declares {}",
            t.name
        );
    }
    assert!(
        capped.contains("\nread_file — "),
        "the read-only tools survive the cap"
    );
}

/// A slot at `no-tools` leaves a fleet that can read the board and change
/// nothing — every head loses every tool, and each says so in words rather than
/// showing an empty list.
#[test]
fn a_no_tools_slot_disarms_every_head() {
    for head in Head::ALL {
        let posting = head.posted(Tier::NoTools);
        assert!(posting.tools().is_empty(), "{posting} kept a tool");
        assert!(
            posting.prefix().contains("You have none"),
            "{posting} shows an empty list instead of saying it has none"
        );
        for t in TOOLS {
            assert!(
                posting.policy().admits(t.name).is_err(),
                "{posting} admits {}",
                t.name
            );
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
    let recon = uncapped(Head::Recon).policy();
    // Recon cannot run git at all: the class check refuses before any argument
    // is looked at.
    assert!(recon.admits("git").is_err());
    // Builders can, and the argument check is what looks at the line.
    assert!(uncapped(Head::Builders).policy().admits("git").is_ok());
    assert_eq!(
        lookup("git").map(|t| t.reach),
        Some(Reach::SpawnsChild),
        "git spawns a child, so it is in the class regardless of its arguments"
    );
}
