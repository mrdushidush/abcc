//! The ladder against real git and real child processes.
//!
//! Every test here builds its own repository, because the structural rung is a
//! claim about what git reports between two snapshots and F328's whole point is
//! that a repository can report one thing and hold another. The toolchain
//! profiles are fakes that need nothing installed — what is under test is the
//! ladder's shape, not cargo.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use abcc_core::outcome::{Headline, Outcome, Reading, Why};
use abcc_engine::workspace::{Standard, Toolchain};
use abcc_gate::{Gate, Rung};
use abcc_vcs::{Repo, Sha, checkpoint_ref};

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
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

/// A repository with one commit, and no toolchain witness in it.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("subject");
    fs::create_dir_all(root.join("src")).expect("mkdir");

    git(dir.path(), &["init", "-q", "-b", "main", "subject"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "test"]);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "first"]);

    (dir, root)
}

fn snapshot(repo: &Repo, seq: i64) -> Sha {
    repo.checkpoint(&checkpoint_ref("m1", seq), &format!("snapshot {seq}"))
        .expect("checkpoint")
}

/// Profiles that need nothing installed. ⚠ They name the platform's own
/// interpreter rather than `bash`, for F492's reason: a toolchain profile spawns
/// the program it names, and on this platform the name `bash` reaches the WSL
/// relay, which answers `execvpe(/bin/bash) failed` at exit 1.
#[cfg(windows)]
fn scripted(test: &'static [&'static str]) -> Toolchain {
    Toolchain {
        name: "scripted",
        witnesses: &[],
        test,
        diagnostics: &["cmd", "/C", "echo checked"],
        reading: Reading::Cargo,
        standard: None,
    }
}

#[cfg(not(windows))]
fn scripted(test: &'static [&'static str]) -> Toolchain {
    Toolchain {
        name: "scripted",
        witnesses: &[],
        test,
        diagnostics: &["sh", "-c", "echo checked"],
        reading: Reading::Cargo,
        standard: None,
    }
}

#[cfg(windows)]
const GREEN_SUITE: &[&str] = &["cmd", "/C", "echo test result: ok. 2 passed; 0 failed;"];
#[cfg(not(windows))]
const GREEN_SUITE: &[&str] = &["sh", "-c", "echo 'test result: ok. 2 passed; 0 failed;'"];

#[cfg(windows)]
const RED_SUITE: &[&str] = &[
    "cmd",
    "/C",
    "echo test result: FAILED. 1 passed; 1 failed; & exit 101",
];
#[cfg(not(windows))]
const RED_SUITE: &[&str] = &[
    "sh",
    "-c",
    "echo 'test result: FAILED. 1 passed; 1 failed;'; exit 101",
];

/// 🚨 A tree whose test target does not compile: cargo exits 101 and **never
/// prints a `test result:` line**. This is F516's runs 22 and 24 in a fixture.
#[cfg(windows)]
const BROKEN_BUILD: &[&str] = &[
    "cmd",
    "/C",
    "echo error[E0433]: failed to resolve & exit 101",
];
#[cfg(not(windows))]
const BROKEN_BUILD: &[&str] = &[
    "sh",
    "-c",
    "echo 'error[E0433]: failed to resolve'; exit 101",
];

#[cfg(windows)]
const NO_TESTS: &[&str] = &["cmd", "/C", "echo test result: ok. 0 passed; 0 failed;"];
#[cfg(not(windows))]
const NO_TESTS: &[&str] = &["sh", "-c", "echo 'test result: ok. 0 passed; 0 failed;'"];

/// Longer than any budget this test gives it.
#[cfg(windows)]
const SLOW: &[&str] = &["cmd", "/C", "ping -n 30 127.0.0.1 > NUL"];
#[cfg(not(windows))]
const SLOW: &[&str] = &["sh", "-c", "sleep 30"];

#[cfg(windows)]
const HOUSE_RULE: &[&str] = &["cmd", "/C", "echo error: a house rule & exit 101"];
#[cfg(not(windows))]
const HOUSE_RULE: &[&str] = &["sh", "-c", "echo 'error: a house rule'; exit 101"];

#[cfg(windows)]
const CLEAN: &[&str] = &["cmd", "/C", "echo clean"];
#[cfg(not(windows))]
const CLEAN: &[&str] = &["sh", "-c", "echo clean"];

/// A second passing command, so a conjunction can be told from a single call.
#[cfg(windows)]
const ALSO_CLEAN: &[&str] = &["cmd", "/C", "echo formatted"];
#[cfg(not(windows))]
const ALSO_CLEAN: &[&str] = &["sh", "-c", "echo formatted"];

const REFUSING_STANDARD: Standard = Standard {
    witnesses: &["standard.toml"],
    commands: &[HOUSE_RULE],
};

const PASSING_STANDARD: Standard = Standard {
    witnesses: &["standard.toml"],
    commands: &[CLEAN],
};

/// 🚨 **F555's shape**: the repository declares two commands and the *first* is
/// the one that refuses. A rung that ran only the last would call this green.
const FIRST_REFUSES: Standard = Standard {
    witnesses: &["standard.toml"],
    commands: &[HOUSE_RULE, CLEAN],
};

/// Both pass, so the green can be asked what it stands on.
const BOTH_PASS: Standard = Standard {
    witnesses: &["standard.toml"],
    commands: &[CLEAN, ALSO_CLEAN],
};

fn with_standard(test: &'static [&'static str], standard: Standard) -> Toolchain {
    Toolchain {
        standard: Some(standard),
        ..scripted(test)
    }
}

fn rungs(measured: &abcc_gate::Measured) -> Vec<&str> {
    measured
        .report
        .outcomes()
        .iter()
        .map(Outcome::rung)
        .collect()
}

// ---------------------------------------------------------------------------
// the structural rung
// ---------------------------------------------------------------------------

/// 🚨 **An attempt that changed nothing is refused, and it is refused for free.**
///
/// This is the commonest ending in this project's whole population: of 25 real
/// attempts on one task, **16 closed on a byte-identical tree**. Every one of
/// them is answered here without spawning a compiler.
#[test]
fn an_unchanged_tree_is_refused_by_the_free_rung_and_nothing_else_runs() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    let after = snapshot(&repo, 2);

    // ⚠ The two snapshots have different commit shas — the timestamp differs —
    // and identical trees. Quoting the shas would make this look like work.
    assert_ne!(
        before, after,
        "two checkpoints of one tree share a commit sha"
    );

    let gate = Gate::open(&repo, &root).with_toolchain(scripted(GREEN_SUITE));
    let measured = gate.measure(&before, &after);

    assert_eq!(measured.refused_by(), Some("structural"));
    assert!(!measured.accepts());
    assert_eq!(
        rungs(&measured),
        vec!["structural"],
        "the ladder ran past the refusal"
    );
    assert!(measured.changed.is_empty());
}

#[test]
fn a_changed_tree_passes_the_free_rung_and_the_detail_names_the_files() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate = Gate::open(&repo, &root).with_toolchain(scripted(GREEN_SUITE));
    let measured = gate.measure(&before, &after);

    let structural = &measured.report.outcomes()[0];
    assert!(structural.is_green());
    match structural {
        Outcome::Measured(m) => assert!(m.detail.contains("src/lib.rs"), "{}", m.detail),
        Outcome::Unmeasured { .. } => panic!("the structural rung did not measure"),
    }
    assert_eq!(measured.changed.len(), 1);
}

// ---------------------------------------------------------------------------
// the conjunction
// ---------------------------------------------------------------------------

/// Every declared rung measured and green. This is the only shape that accepts,
/// and it is `Headline::is_pass` saying so rather than anything in this crate.
#[test]
fn green_needs_every_declared_rung_and_that_is_the_whole_conjunction() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate = Gate::open(&repo, &root).with_toolchain(scripted(GREEN_SUITE));
    let measured = gate.measure(&before, &after);

    assert_eq!(rungs(&measured), vec!["structural", "acceptance", "veto"]);
    assert_eq!(
        measured.headline,
        Headline::Green { rungs: 3 },
        "{}",
        measured.headline
    );
    assert!(measured.accepts());
}

/// 🚨 **A rung that could not run does not disappear and does not become a
/// failure.** The headline says exactly what was missing, which is the outcome
/// the donors do not have — v1's dispatcher cannot tell *every test failed* from
/// *there were no tests*.
#[test]
fn a_workspace_with_no_profile_is_unverified_and_says_what_was_missing() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let after = snapshot(&repo, 2);

    // No `with_toolchain`, and the fixture has no witness file in it.
    let measured = Gate::open(&repo, &root).measure(&before, &after);

    assert!(!measured.accepts());
    assert_eq!(measured.refused_by(), None, "an absence is not a refusal");
    match &measured.headline {
        Headline::Unverified { missing } => {
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].0, "acceptance");
            assert!(matches!(missing[0].1, Why::NoCheckerForArtifact { .. }));
        }
        other => panic!("expected unverified, got {other}"),
    }
    // The absence did not stop the ladder: the veto still ran and still passed.
    assert_eq!(rungs(&measured), vec!["structural", "acceptance", "veto"]);
}

/// ADR-0009 §2's *common awkward case*, spelled out: a suite that ran and found
/// nothing is **not** a pass and **not** a veto. `Green` is a claim about
/// coverage, so this is `Unverified` and the console line says why.
#[test]
fn a_suite_that_found_nothing_to_run_is_unverified_rather_than_vetoed() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate = Gate::open(&repo, &root).with_toolchain(scripted(NO_TESTS));
    let measured = gate.measure(&before, &after);

    assert!(!measured.accepts());
    assert_eq!(measured.refused_by(), None);
    assert!(
        measured.headline.to_string().contains("nothing to run"),
        "{}",
        measured.headline
    );
}

#[test]
fn a_red_suite_stops_the_ladder_at_the_acceptance_rung() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate = Gate::open(&repo, &root).with_toolchain(scripted(RED_SUITE));
    let measured = gate.measure(&before, &after);

    assert_eq!(measured.refused_by(), Some("acceptance"));
    assert_eq!(rungs(&measured), vec!["structural", "acceptance"]);
}

// ---------------------------------------------------------------------------
// the veto
// ---------------------------------------------------------------------------

/// 🚨 **F516: a tree changed by a cut run does not compile.** Two of the three
/// tree-changing runs at the raised budget left non-building trees, because both
/// ended mid-edit — so *tree changed* is not *work done*, and the veto is where
/// that is enforced instead of remembered.
///
/// The acceptance rung's own outcome is checked here too: it stays the absence it
/// measured. Nothing is rewritten; a second rung reaches a second conclusion.
#[test]
fn a_tree_that_does_not_build_is_vetoed_and_the_absence_is_still_on_the_record() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 {\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate = Gate::open(&repo, &root).with_toolchain(scripted(BROKEN_BUILD));
    let measured = gate.measure(&before, &after);

    assert_eq!(measured.refused_by(), Some("veto"));
    assert!(
        measured.headline.to_string().contains("does not build"),
        "{}",
        measured.headline
    );
    let acceptance = &measured.report.outcomes()[1];
    assert!(
        matches!(
            acceptance,
            Outcome::Unmeasured {
                why: Why::FailedBeforeRunning { .. },
                ..
            }
        ),
        "the veto rewrote the rung it read: {acceptance:?}"
    );
}

// ---------------------------------------------------------------------------
// the standard
// ---------------------------------------------------------------------------

/// 🚨 **An undeclared rung is absent, not missing.** A workspace with no witness
/// has three rungs and a `Green` that means what it says — it does not carry a
/// fourth `Unmeasured` for a standard nobody asked for.
#[test]
fn the_standard_rung_is_declared_by_the_repository_and_not_by_us() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let toolchain = with_standard(GREEN_SUITE, REFUSING_STANDARD);

    let undeclared = Gate::open(&repo, &root).with_toolchain(toolchain);
    assert_eq!(undeclared.standard(), None);
    assert_eq!(
        undeclared.ladder(),
        vec![Rung::Structural, Rung::Acceptance, Rung::Veto]
    );

    fs::write(root.join("standard.toml"), "# this repository asks\n").expect("write");
    let declared = Gate::open(&repo, &root).with_toolchain(toolchain);
    assert_eq!(declared.standard(), Some(REFUSING_STANDARD));
    assert_eq!(declared.ladder().len(), 4);
}

/// 🚨 **The Gate demonstrated before the Gate existed, as a test.** F512: six
/// working `--version` implementations by the champion, and the repository's own
/// declared standard refuses all six — on two of them (runs 10 and 23) every test
/// passes, so the standard is the *only* rung that refuses them.
///
/// That shape is what this asserts: a green suite, a refusing standard, and a
/// headline that names the standard rather than the tests. The live version
/// against the real shas is in `tests/live.rs`.
#[test]
fn a_tree_whose_tests_pass_is_still_refused_by_the_standard_it_declares() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    fs::write(root.join("standard.toml"), "# this repository asks\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate =
        Gate::open(&repo, &root).with_toolchain(with_standard(GREEN_SUITE, REFUSING_STANDARD));
    let measured = gate.measure(&before, &after);

    assert_eq!(
        rungs(&measured),
        vec!["structural", "acceptance", "veto", "standard"]
    );
    assert_eq!(measured.refused_by(), Some("standard"));
    assert!(!measured.accepts());
    // The acceptance rung is green, and stays green. The refusal is not a
    // statement that the work is wrong; it is a statement that it cannot land.
    assert!(measured.report.outcomes()[1].is_green());
}

#[test]
fn four_declared_rungs_all_green_is_a_green_of_four() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    fs::write(root.join("standard.toml"), "# this repository asks\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate =
        Gate::open(&repo, &root).with_toolchain(with_standard(GREEN_SUITE, PASSING_STANDARD));
    let measured = gate.measure(&before, &after);

    assert_eq!(measured.headline, Headline::Green { rungs: 4 });
    assert!(measured.accepts());
}

/// 🚨🚨 **F555, as a regression test: the standard is a conjunction, and the
/// FIRST command's refusal is the rung's refusal.**
///
/// The finding was a run reaching MISSION ACCOMPLISHED on a tree
/// `cargo fmt --check` refuses — all four rungs green, the Judge with no
/// findings, and rustfmt in none of the rungs. The repair is that a standard
/// holds a *list*, so the shape that has to hold is this one: the command that
/// refuses is the one that runs first, and a rung that only looked at the last
/// answer would report green.
///
/// ⚠ The detail is asserted, not just the exit. `standard: exit 1` that does not
/// name its command sends a reader to the compiler for a formatting refusal.
#[test]
fn the_first_command_of_a_declared_standard_can_refuse_it() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    fs::write(root.join("standard.toml"), "# this repository asks\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate = Gate::open(&repo, &root).with_toolchain(with_standard(GREEN_SUITE, FIRST_REFUSES));
    let measured = gate.measure(&before, &after);

    assert_eq!(measured.refused_by(), Some("standard"));
    assert!(!measured.accepts());

    let Outcome::Measured(m) = &measured.report.outcomes()[3] else {
        panic!("the standard rung produced no measurement");
    };
    assert_ne!(m.exit, 0);
    assert!(
        m.detail.contains(&HOUSE_RULE.join(" ")),
        "a refused standard does not say which of its commands refused: {:?}",
        m.detail
    );
    assert!(
        m.detail.contains("a house rule"),
        "the refusing command's own output was dropped: {:?}",
        m.detail
    );
}

/// **A green standard names every command it stands on.**
///
/// The counterpart to the test above, and it is the sentence F555 caught lying:
/// a green that says only `standard` is a green an operator cannot check. This
/// one has to be able to show that both commands ran.
#[test]
fn a_green_standard_names_every_command_it_passed() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    fs::write(root.join("standard.toml"), "# this repository asks\n").expect("write");
    let after = snapshot(&repo, 2);

    let gate = Gate::open(&repo, &root).with_toolchain(with_standard(GREEN_SUITE, BOTH_PASS));
    let measured = gate.measure(&before, &after);

    assert_eq!(measured.headline, Headline::Green { rungs: 4 });
    let Outcome::Measured(m) = &measured.report.outcomes()[3] else {
        panic!("the standard rung produced no measurement");
    };
    for argv in [CLEAN, ALSO_CLEAN] {
        assert!(
            m.detail.contains(&argv.join(" ")),
            "the green does not name `{}`: {:?}",
            argv.join(" "),
            m.detail
        );
    }
}

// ---------------------------------------------------------------------------
// what the host could not do
// ---------------------------------------------------------------------------

/// ⚠ A checker that is not on this host is an absence with a name, never a
/// failure of the work — and never a pass. A name that resolves is not a working
/// interpreter (F312), so the probe is the run.
#[test]
fn a_checker_that_is_not_here_is_named_rather_than_failed() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let after = snapshot(&repo, 2);

    let absent = scripted(&["abcc-no-such-checker-exists"]);
    let measured = Gate::open(&repo, &root)
        .with_toolchain(absent)
        .measure(&before, &after);

    assert!(!measured.accepts());
    assert_eq!(measured.refused_by(), None);
    assert!(
        matches!(
            &measured.report.outcomes()[1],
            Outcome::Unmeasured {
                why: Why::CheckerNotOnHost { .. },
                ..
            }
        ),
        "{:?}",
        measured.report.outcomes()[1]
    );
}

/// A rung that outran its budget is [`Why::Timeout`] and never an exit status. A
/// timeout folded into "clean" is the worst available lie, because the process
/// may still be alive (F220).
#[test]
fn a_rung_that_outruns_its_budget_is_a_timeout_and_not_a_red_suite() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = snapshot(&repo, 1);
    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    let after = snapshot(&repo, 2);

    let measured = Gate::open(&repo, &root)
        .with_toolchain(scripted(SLOW))
        .with_budget(Duration::from_millis(300))
        .measure(&before, &after);

    assert!(matches!(
        &measured.report.outcomes()[1],
        Outcome::Unmeasured {
            why: Why::Timeout { .. },
            ..
        }
    ));
    assert_eq!(measured.refused_by(), None, "a timeout is not a refusal");
}
