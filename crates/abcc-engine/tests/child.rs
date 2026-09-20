//! The tool child, against real processes.
//!
//! Every claim here is about what an OS actually does, so nothing is simulated:
//! F202's defect is a pipe buffer filling up, and a fake pipe does not have one.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use abcc_core::event::Control;
use abcc_core::outcome::Why;
use abcc_engine::child::{ENV_ALLOWLIST, Spawn, ToolChild};
use abcc_engine::control::ControlPoint;
use abcc_engine::tools::Confinement;

/// A shell invocation, per platform. The two branches exist because the claims
/// under test are about processes, and a process needs a real interpreter.
fn shell(cwd: &Path, script: &str) -> Spawn {
    if cfg!(windows) {
        Spawn::new("cmd", cwd).arg("/C").arg(script)
    } else {
        Spawn::new("sh", cwd).arg("-c").arg(script)
    }
}

fn sleeper(cwd: &Path) -> Spawn {
    if cfg!(windows) {
        // `ping` is the portable Windows sleep; 60 pings is about a minute.
        shell(cwd, "ping -n 60 127.0.0.1")
    } else {
        shell(cwd, "sleep 60")
    }
}

/// The same sleep with no interpreter in front of it, so nothing but the child
/// itself holds the pipe. It is the control in the halt-latency measurement.
fn bare_sleeper(cwd: &Path) -> Spawn {
    if cfg!(windows) {
        Spawn::new("ping", cwd).args(["-n", "60", "127.0.0.1"])
    } else {
        Spawn::new("sleep", cwd).arg("60")
    }
}

fn dump_env(cwd: &Path) -> Spawn {
    if cfg!(windows) {
        shell(cwd, "set")
    } else {
        shell(cwd, "env")
    }
}

// ---------------------------------------------------------------------------
// F202
// ---------------------------------------------------------------------------

/// 🚨 **The defect this module exists for.** A child that writes past the 64 KiB
/// pipe buffer blocks on `write`, and the donor records it as a *timeout* for a
/// tool that had finished its work. One mebibyte is sixteen buffers.
#[test]
fn a_child_that_outruns_the_pipe_buffer_is_not_a_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = dir.path().join("big.txt");
    let payload = "x".repeat(1024 * 1024);
    fs::write(&big, &payload).expect("write");

    let script = if cfg!(windows) {
        format!("type {}", big.display())
    } else {
        format!("cat {}", big.display())
    };
    let child = ToolChild::spawn(&shell(dir.path(), &script).budget(Duration::from_secs(30)))
        .expect("spawn");
    let finished = child.finish();

    assert_eq!(
        finished.unmeasured, None,
        "recorded as unmeasured: {finished:?}"
    );
    assert_eq!(finished.exit, Some(0));
    assert!(
        finished.stdout.len() >= 1024 * 1024,
        "captured only {} bytes of a 1 MiB stream",
        finished.stdout.len()
    );
}

// ---------------------------------------------------------------------------
// The environment
// ---------------------------------------------------------------------------

/// ADR-0014 §5: `env_clear()` plus an allowlist, because **one donor of three
/// strips anything**.
///
/// It checks the whole parent environment rather than one planted marker, which
/// is both the stronger claim and the only one available: the workspace forbids
/// `unsafe`, and `std::env::set_var` is unsafe in this edition. Every variable
/// this process holds that is not on the list must be absent from the child by
/// name — a name is enough, because a name that survives means the value did.
#[test]
fn the_operators_environment_does_not_reach_the_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let finished = ToolChild::spawn(&dump_env(dir.path()).budget(Duration::from_secs(30)))
        .expect("spawn")
        .finish();
    assert_eq!(finished.unmeasured, None);

    // A positive control in the same measurement: without it, an empty dump
    // would pass every assertion below for the wrong reason (F45).
    let dumped: Vec<String> = finished
        .stdout
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_ascii_uppercase()))
        .collect();
    assert!(
        dumped.iter().any(|k| k == "PATH"),
        "the dump has no PATH, so the negative results below prove nothing: {}",
        finished.stdout
    );

    let allowed: Vec<String> = ENV_ALLOWLIST
        .iter()
        .map(|k| k.to_ascii_uppercase())
        .collect();
    let mut leaked: Vec<String> = Vec::new();
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy().to_ascii_uppercase();
        if !allowed.contains(&name) && dumped.contains(&name) {
            leaked.push(name);
        }
    }
    assert!(
        leaked.is_empty(),
        "these reached the child without being on the allowlist: {leaked:?}"
    );

    // And one by name, so the test reads as the claim it makes. Cargo sets this
    // in every test process and it is deliberately not on the list.
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "the control variable is not set in this process"
    );
    assert!(!dumped.iter().any(|k| k == "CARGO_MANIFEST_DIR"));
}

/// The allowlist is data in one const, so growing it is a visible edit rather
/// than a side effect of making something work.
#[test]
fn the_allowlist_is_a_named_list_with_no_prefix_rules() {
    assert!(ENV_ALLOWLIST.contains(&"PATH"));
    assert!(!ENV_ALLOWLIST.iter().any(|k| k.contains('*')));
    let mut sorted: Vec<&str> = ENV_ALLOWLIST.to_vec();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "duplicate entry in ENV_ALLOWLIST");
    for key in ["AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "OPENAI_API_KEY"] {
        assert!(
            !ENV_ALLOWLIST.contains(&key),
            "{key} must never be admitted by name"
        );
    }
}

// ---------------------------------------------------------------------------
// Endings that are not exit codes
// ---------------------------------------------------------------------------

/// ⚠ A timeout folded into "clean" is the worst available lie, because the
/// process may still be alive (F220). It is [`Why::Timeout`] and there is no exit
/// code, because there was no ending to read one from.
#[test]
fn a_child_that_outstays_its_budget_is_a_timeout_and_never_an_exit_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let finished = ToolChild::spawn(&sleeper(dir.path()).budget(Duration::from_millis(300)))
        .expect("spawn")
        .finish();

    assert_eq!(
        finished.exit, None,
        "a timeout has no exit status to report"
    );
    match finished.unmeasured {
        Some(Why::Timeout { after_ms }) => {
            assert!(
                after_ms >= 300,
                "reported {after_ms} ms against a 300 ms budget"
            );
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

/// 🚨 A child the operator stopped is **not** a child that failed.
/// `TerminateProcess` hands back exit code 1, and a run recording that would be
/// saying *the tests failed* about work nobody ran.
#[test]
fn a_killed_child_is_cancelled_rather_than_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let child =
        ToolChild::spawn(&sleeper(dir.path()).budget(Duration::from_mins(1))).expect("spawn");
    let killer = child.killer();
    assert!(killer.pid() > 0);

    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        killer.kill("operator").expect("kill");
    });

    let finished = child.finish();
    stopper.join().expect("the killer thread did not panic");

    assert_eq!(finished.exit, None);
    assert_eq!(
        finished.unmeasured,
        Some(Why::Cancelled {
            by: "operator".to_owned()
        })
    );
    assert!(
        finished.elapsed_ms < 30_000,
        "the kill did not take effect: {} ms",
        finished.elapsed_ms
    );
}

/// Two different things to tell an operator, so two values. And the only honest
/// probe for the second is to execute — a name that resolves is not a working
/// interpreter (F312).
#[test]
fn a_program_that_is_not_here_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spawn = Spawn::new("abcc-no-such-binary-6f2c", dir.path());
    match ToolChild::spawn(&spawn) {
        Err(Why::CheckerNotOnHost { binary }) => assert!(binary.contains("abcc-no-such-binary")),
        Err(other) => panic!("expected CheckerNotOnHost, got {other:?}"),
        Ok(_) => panic!("a binary that does not exist was spawned"),
    }
}

// ---------------------------------------------------------------------------
// The posture
// ---------------------------------------------------------------------------

/// 🚨 ADR-0014 §3: confinement is a value the console shows, not a Boolean and
/// not an argument silently ignored on the wrong platform. **Every child on this
/// platform reports `Cwd`, and that is the true state of 2.0 today.**
#[test]
fn every_child_reports_the_confinement_it_actually_got() {
    let dir = tempfile::tempdir().expect("tempdir");
    let finished = ToolChild::spawn(&shell(dir.path(), "cd").budget(Duration::from_secs(30)))
        .expect("spawn")
        .finish();
    assert_eq!(finished.confinement, Confinement::Cwd);
}

/// The child runs in the tree it was given. Inheriting the caller's working
/// directory is the one mistake that turns a blast radius into the machine.
#[test]
fn the_child_runs_where_it_was_put() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker-8b1e.txt");
    fs::write(&marker, "here").expect("write");

    let script = if cfg!(windows) { "dir /B" } else { "ls -1" };
    let finished = ToolChild::spawn(&shell(dir.path(), script).budget(Duration::from_secs(30)))
        .expect("spawn")
        .finish();

    assert_eq!(finished.unmeasured, None);
    assert!(
        finished.stdout.contains("marker-8b1e.txt"),
        "listed a different directory: {}",
        finished.stdout
    );
}

// ---------------------------------------------------------------------------
// The operator's verb, while a tool runs
// ---------------------------------------------------------------------------

/// 🚨 **A `Halt` issued while `run_tests` runs must not wait for `run_tests`.**
///
/// A generation is cancelled by dropping its stream, which closes the socket
/// (F200). A tool child has no such property, and this is the whole of
/// ADR-0006's justification for `shared_child`: the worker polls its control
/// flag on its own thread — no third thread per tool — and kills the child by
/// name.
///
/// 🚨 **Both shapes are timed in one test on purpose (F491).** The bare child is
/// the control: it is the same verb, the same poll and the same kill, and the
/// only difference is whether a grandchild is holding the pipe. Timing the
/// wrapped one alone would have read as *the poll interval is 2 s*, which it is
/// not.
#[test]
fn an_operators_verb_reaches_a_running_tool_child() {
    // The control: one process, so the pipes reach EOF the moment it dies.
    let (bare, bare_ms) = halted(&bare_sleeper);
    // The real shape: `bash -c` leaves a grandchild holding the write end.
    let (wrapped, wrapped_ms) = halted(&sleeper);

    for finished in [&bare, &wrapped] {
        assert_eq!(finished.exit, None, "a stopped child has no exit status");
        assert_eq!(
            finished.unmeasured,
            Some(Why::Cancelled {
                by: "operator".to_owned()
            }),
            "a child the operator stopped is not a child that failed"
        );
    }
    // Loose bounds, because these are real processes on a shared box; the
    // numbers themselves are printed and quoted in `WATCH_POLL` and
    // `DRAIN_GRACE`.
    assert!(
        bare_ms < 1_000,
        "the verb took {bare_ms} ms to reach a bare child"
    );
    assert!(
        wrapped_ms < 5_000,
        "the verb took {wrapped_ms} ms to reach a wrapped child"
    );
    println!("halt to result: bare child {bare_ms} ms, shell-wrapped child {wrapped_ms} ms");
}

/// Start a sleeper, halt it 150 ms in, and time the verb against the result the
/// worker actually gets back.
fn halted(shape: &dyn Fn(&Path) -> Spawn) -> (abcc_engine::child::Finished, u128) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (control, handle) = ControlPoint::new();
    let child = ToolChild::spawn(&shape(dir.path()).budget(Duration::from_mins(1))).expect("spawn");

    let poked = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        let at = Instant::now();
        handle
            .request(Control::Halt)
            .expect("the worker is still there");
        at
    });

    let finished = child.finish_watching(&control.watch());
    let asked_at = poked.join().expect("the console thread did not panic");
    (finished, asked_at.elapsed().as_millis())
}

/// A watch with nothing on it changes nothing. The tool ends the way it would
/// have, and the wait is one call rather than a poll loop.
#[test]
fn a_watch_nobody_pokes_leaves_the_result_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (control, _handle) = ControlPoint::new();
    let finished =
        ToolChild::spawn(&shell(dir.path(), "echo watched").budget(Duration::from_secs(30)))
            .expect("spawn")
            .finish_watching(&control.watch());

    assert_eq!(finished.exit, Some(0));
    assert_eq!(finished.unmeasured, None);
    assert!(finished.stdout.contains("watched"), "{}", finished.stdout);
}

/// 🚨 F356, as a maintenance test rather than a memory. Two trees holding one
/// package name and one shared `CARGO_TARGET_DIR` make cargo print `Fresh`, run
/// *the other tree's* binary and report `ok. 0 passed` at exit 0 — a green test
/// run that measured nothing. The allowlist is what keeps a tool child from
/// inheriting one, so the guard belongs where the list is.
#[test]
fn the_allowlist_does_not_carry_a_shared_build_cache() {
    for forbidden in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        assert!(
            !ENV_ALLOWLIST.contains(&forbidden),
            "{forbidden} would let two trees share one gate (F356)"
        );
    }
}

/// 🚨 **The allowlist must carry a home the platform actually uses, and on
/// Windows that is not `HOME`.**
///
/// This is the other half of the claim above, and it is the half that was
/// missing. `env_clear()` plus an allowlist is only correct if the allowlist
/// admits what the workload needs; the workload is *build and test an existing
/// repository*, and a test suite that cannot resolve a home directory fails for
/// a reason that has nothing to do with the tree being graded.
///
/// Found against `claudette`: the gate recorded `1161 run, 1103 passed, 58
/// failed` on a checkpoint whose tree passes **1161 of 1161** when run with a
/// full environment. Clearing exactly `USERPROFILE`, `APPDATA` and
/// `LOCALAPPDATA` reproduces **57** of those failures. `HOME` was empty on that
/// machine, so before this the child had no home at all — the rung was
/// manufacturing failures and charging them to the model.
#[test]
fn the_child_gets_a_home_the_platform_can_find() {
    let dir = tempfile::tempdir().expect("tempdir");
    let finished = ToolChild::spawn(&dump_env(dir.path()).budget(Duration::from_secs(30)))
        .expect("spawn")
        .finish();
    assert_eq!(finished.unmeasured, None);

    let dumped: Vec<String> = finished
        .stdout
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_ascii_uppercase()))
        .collect();
    // The same positive control the test above uses: an empty dump would
    // satisfy nothing below for the wrong reason.
    assert!(
        dumped.iter().any(|k| k == "PATH"),
        "the dump has no PATH, so what follows proves nothing: {}",
        finished.stdout
    );

    // Whichever variable *this* platform uses to say where home is, the parent
    // has it and the child must too. Asked of the parent rather than hardcoded,
    // because a machine that does not set it cannot be failed for not passing
    // it on.
    let homes: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "APPDATA", "LOCALAPPDATA"]
    } else {
        &["HOME"]
    };
    for name in homes {
        if std::env::var_os(name).is_some() {
            assert!(
                dumped.contains(&(*name).to_ascii_uppercase()),
                "the parent has {name} and the child did not get it, so any suite that \
                 resolves a home directory will fail for a reason the tree did not cause"
            );
        }
    }
}
