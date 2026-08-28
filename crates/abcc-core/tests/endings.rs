//! Run the outcome type against the same bytes the donors' parsers were given.
//!
//! `tests/captured/` holds what pytest, `python -m unittest` and `cargo test`
//! actually printed in a scratch tree, together with the exit status the OS
//! returned. Nothing is re-typed by hand and the file names carry the ground
//! truth.
//!
//! The question is not *does this agree with a donor* — it is **how many of the
//! eight real endings the record can still tell apart when it is read back.**
//! v1 gets three. The donor comparison itself lives in the research repository,
//! where the donor's own code may be quoted; this repository ships no inherited
//! code, so what is asserted here is our behaviour on the same bytes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use abcc_core::outcome::{
    Claim, Headline, Outcome, Report, Why, classify_cargo_tests, classify_python_tests,
};

/// A stand-in for the snapshot sha a real measurement is stamped with
/// (ADR-0009 §6). These captures predate the tree they were taken from, so the
/// value only has to be consistent within a test.
const SHA: &str = "0000000000000000000000000000000000000000";

fn captured_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("captured")
}

/// (case slug, exit code, output) for one runner family.
fn endings_with(prefix: &str) -> Vec<(String, i32, String)> {
    let dir = captured_dir();
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).expect("tests/captured/") {
        let p = entry.expect("dir entry").path();
        if p.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let slug = p.file_stem().unwrap().to_string_lossy().to_string();
        if !slug.starts_with(prefix) {
            continue;
        }
        let exit: i32 = fs::read_to_string(p.with_extension("exit"))
            .expect("matching .exit file")
            .trim()
            .parse()
            .expect("exit code");
        out.push((slug, exit, fs::read_to_string(&p).expect("output")));
    }
    out.sort();
    assert!(
        !out.is_empty(),
        "no {prefix} captures found in {}",
        dir.display()
    );
    out
}

/// The eight pytest / unittest endings.
fn endings() -> Vec<(String, i32, String)> {
    let mut v = endings_with("pytest__");
    v.extend(endings_with("unittest__"));
    v.sort();
    v
}

/// The five cargo endings.
fn cargo_endings() -> Vec<(String, i32, String)> {
    endings_with("cargo__")
}

#[test]
fn every_capture_is_classified_and_the_classes_are_not_all_the_same() {
    let mut classes = BTreeSet::new();
    for (slug, exit, out) in endings() {
        let outcome = classify_python_tests("tests", SHA, exit, &out);
        let class = match &outcome {
            Outcome::Measured(m) => format!(
                "measured/exit={}/counts={:?}",
                m.exit,
                m.counts.map(|c| (c.run, c.passed, c.failed))
            ),
            Outcome::Unmeasured { why, .. } => format!("unmeasured/{why:?}")
                .split(" {")
                .next()
                .unwrap()
                .to_string(),
        };
        println!("{slug:32} exit={exit}  ->  {class}");
        classes.insert(class);
    }
    // v1's parser returns the same object for "3 of 3 failed", "nothing to
    // collect" and "collection error". These must not.
    assert!(
        classes.len() >= 5,
        "the eight endings collapsed to {} classes: {classes:?}",
        classes.len()
    );
}

#[test]
fn a_run_that_collected_nothing_is_never_green() {
    for (slug, exit, out) in endings() {
        if !slug.contains("nothing-to-collect") {
            continue;
        }
        let outcome = classify_python_tests("tests", SHA, exit, &out);
        assert!(!outcome.is_green(), "{slug} classified green");
        assert!(!outcome.is_red(), "{slug} classified red — it is neither");
        assert!(
            matches!(
                outcome,
                Outcome::Unmeasured {
                    why: Why::NothingToRun { .. },
                    ..
                }
            ),
            "{slug}: {outcome:?}"
        );

        // And the report it lands in cannot be a pass, however loudly the model
        // says otherwise.
        let mut r = Report::new();
        r.record(outcome).note(Claim {
            by: "coder".into(),
            text: "Implemented the upload handler. All tests pass.".into(),
        });
        assert!(!r.headline().is_pass(), "{slug}: {}", r.headline());
        println!("{slug:32} -> {}", r.headline());
    }
}

#[test]
fn the_all_red_run_is_measured_and_red_and_carries_its_counts() {
    let (slug, exit, out) = endings()
        .into_iter()
        .find(|(s, _, _)| s.contains("3-of-3-fail"))
        .expect("the all-red capture");
    let outcome = classify_python_tests("tests", SHA, exit, &out);
    assert!(outcome.is_red(), "{slug}: {outcome:?}");
    let Outcome::Measured(m) = &outcome else {
        panic!("{slug}: {outcome:?}")
    };
    let c = m.counts.expect("counts");
    assert_eq!((c.run, c.passed, c.failed), (3, 0, 3), "{slug}");
}

#[test]
fn the_mixed_run_counts_are_right_in_both_directions() {
    // v1 reports 2 run / 0 passed / 2 failed for this one, because real pytest
    // prints "2 failed, 1 passed" and its regex wants `passed` first.
    let (slug, exit, out) = endings()
        .into_iter()
        .find(|(s, _, _)| s.contains("1-passes"))
        .expect("the mixed capture");
    let Outcome::Measured(m) = classify_python_tests("tests", SHA, exit, &out) else {
        panic!("{slug}: expected a measurement")
    };
    let c = m.counts.expect("counts");
    assert_eq!((c.run, c.passed, c.failed), (3, 1, 2), "{slug}");
}

/// The regression this type's own first draft shipped: unittest prints its
/// breakdown as `FAILED (failures=2)`, which is not a `<n> <word>` pair, so a
/// scanner that only reads pairs sees a total of 2 and no failures and reports
/// two passes on an all-red run.
#[test]
fn a_red_unittest_run_never_reports_a_pass_it_did_not_see() {
    let (slug, exit, out) = endings()
        .into_iter()
        .find(|(s, _, _)| s.contains("unittest__2-of-2-fail"))
        .expect("the unittest all-red capture");
    let outcome = classify_python_tests("tests", SHA, exit, &out);
    assert!(outcome.is_red(), "{slug}: {outcome:?}");
    let Outcome::Measured(m) = &outcome else {
        panic!("{slug}: {outcome:?}")
    };
    match m.counts {
        Some(c) => assert_eq!((c.run, c.passed, c.failed), (2, 0, 2), "{slug}"),
        None => panic!("{slug}: unittest printed failures=2 and it was not read"),
    }
}

#[test]
fn a_green_headline_needs_every_rung_measured() {
    let (_, exit, out) = endings()
        .into_iter()
        .find(|(s, _, _)| s.contains("pytest__2-of-2-pass"))
        .expect("the green capture");

    let mut r = Report::new();
    r.record(classify_python_tests("tests", SHA, exit, &out));
    assert!(r.headline().is_pass(), "{}", r.headline());

    // Add one rung that could not run and the headline stops being a pass —
    // without becoming a failure.
    r.record(Outcome::Unmeasured {
        rung: "lint".into(),
        why: Why::CheckerNotOnHost {
            binary: "ruff".into(),
        },
    });
    let h = r.headline();
    assert!(!h.is_pass(), "{h}");
    assert!(matches!(h, Headline::Unverified { .. }), "{h}");
    assert_eq!(r.measured_fraction(), (1, 2));
    println!("{h}");
}

/// The case that makes the design concrete. `cargo test` on a crate with no
/// tests exits **0** and prints `test result: ok. 0 passed`, and the best donor
/// in the family returns `Some(true)` with the line "tests: 0 passed" for it
/// (F357). The same bytes and the same exit status here produce an outcome that
/// cannot be rendered as a pass.
#[test]
fn a_cargo_crate_with_no_tests_is_unmeasured_rather_than_green() {
    let (slug, exit, out) = cargo_endings()
        .into_iter()
        .find(|(s, _, _)| s.contains("no-tests-at-all"))
        .expect("the cargo no-tests capture");
    assert_eq!(exit, 0, "{slug}: cargo really does exit 0 here");
    let outcome = classify_cargo_tests("tests", SHA, exit, &out);
    println!("{slug:28} exit={exit} -> {outcome:?}");
    assert!(
        matches!(
            outcome,
            Outcome::Unmeasured {
                why: Why::NothingToRun { .. },
                ..
            }
        ),
        "{outcome:?}"
    );

    let mut r = Report::new();
    r.record(outcome);
    assert!(!r.headline().is_pass(), "{}", r.headline());
    println!("{}", r.headline());
}

#[test]
fn the_five_cargo_endings_are_four_outcomes_and_the_green_one_is_the_only_pass() {
    let mut passes = 0;
    for (slug, exit, out) in cargo_endings() {
        let o = classify_cargo_tests("tests", SHA, exit, &out);
        let mut r = Report::new();
        r.record(o.clone());
        let h = r.headline();
        println!("{slug:28} exit={exit:<4} -> {h}");
        if h.is_pass() {
            passes += 1;
            assert!(
                slug.contains("2-of-2-pass"),
                "{slug} passed and should not have"
            );
        }
    }
    assert_eq!(passes, 1);
}

/// ADR-0009 §6. 6 of 81 truthful, measured, green claims in Phase 1 described a
/// tree that no longer existed, because the edit landed 1.7–3.0 s after the test
/// binary. A sentence cannot carry that; a sha can.
#[test]
fn a_green_measurement_at_a_stale_sha_is_unverified_and_not_green() {
    let (_, exit, out) = endings()
        .into_iter()
        .find(|(s, _, _)| s.contains("pytest__2-of-2-pass"))
        .expect("the green capture");
    let mut r = Report::new();
    r.record(classify_python_tests("tests", SHA, exit, &out));

    assert!(r.headline_at(SHA).is_pass(), "same sha should still pass");

    let h = r.headline_at("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    assert!(!h.is_pass(), "{h}");
    assert!(matches!(h, Headline::Unverified { .. }), "{h}");
    println!("{h}");
}

/// A red measurement taken at an older sha is still a red measurement. The fault
/// was really observed, and the tree moving afterwards does not un-observe it.
#[test]
fn a_red_measurement_at_a_stale_sha_stays_red() {
    let (_, exit, out) = endings()
        .into_iter()
        .find(|(s, _, _)| s.contains("pytest__3-of-3-fail"))
        .expect("the all-red capture");
    let mut r = Report::new();
    r.record(classify_python_tests("tests", SHA, exit, &out));

    let h = r.headline_at("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    assert!(matches!(h, Headline::Red { .. }), "{h}");
}
