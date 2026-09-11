//! The weights pin: ADR-0014 §6's *record the digest at first pull and compare
//! it on every start*, and the two things it deliberately does not claim.
//!
//! 🚨 **Nothing here reads a real model.** A full digest of the champion is
//! **51.9 s** (12.67 GiB at ~250 MB/s, measured 2026-09-11), which is the number
//! the whole design turns on and exactly the reason the suite must not pay it.
//! These files are bytes long and the properties are the same ones.

use std::fs;
use std::path::{Path, PathBuf};

use abcc::weights::{Effort, Identity, Pins, check, digest_of, pin_path, reads_the_file};
use abcc_core::event::WeightsOutcome;

/// A model id that *is* an absolute path, which `locate` accepts as its second
/// source — so these tests need neither `ABCC_MODEL_WEIGHTS` nor a models
/// directory, and therefore never touch the real user profile.
fn weights_file(dir: &Path, body: &[u8]) -> String {
    let path = dir.join("model.gguf");
    fs::write(&path, body).expect("write the fake weights");
    path.display().to_string()
}

fn touch(path: &str, body: &[u8]) {
    fs::write(path, body).expect("rewrite the fake weights");
}

/// The first sight records the bytes. ⚠ And `Pinned` is **not** a pass: it is
/// the arm that says *this is now the reference*, which is a different claim
/// from *this is what it was before*.
#[test]
fn a_first_sight_pins_the_digest_and_says_that_is_what_it_did() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"the weights, such as they are");
    let mut pins = Pins::default();

    let checked = check(&mut pins, &model, Effort::Cheap, 1_700_000_000_000);

    assert_eq!(checked.outcome, WeightsOutcome::Pinned);
    assert!(checked.pin.is_some(), "the caller was not told to save");
    assert_eq!(pins.len(), 1);
    let pin = pins.get(&model).expect("pinned");
    assert_eq!(pin.digest, checked.digest.clone().expect("a digest"));
    assert_eq!(pin.pinned_at_ms, 1_700_000_000_000);
}

/// 🚨 **The two passing arms are not the same claim, and this is the test that
/// holds them apart.** `Verified` read 12.67 GiB; `Unchanged` read a directory
/// entry. Collapsing them would make every start look like a full check — F495's
/// mistake (a listed model reading as a loaded one), one asset over.
#[test]
fn a_cheap_check_and_a_full_one_report_different_words_for_the_same_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"unchanged bytes");
    let mut pins = Pins::default();
    check(&mut pins, &model, Effort::Full, 1);

    let cheap = check(&mut pins, &model, Effort::Cheap, 2);
    assert_eq!(cheap.outcome, WeightsOutcome::Unchanged);
    assert!(
        cheap.pin.is_none(),
        "the cheap path rewrote the pin it did not verify"
    );

    let full = check(&mut pins, &model, Effort::Full, 3);
    assert_eq!(full.outcome, WeightsOutcome::Verified);
    assert_eq!(cheap.digest, full.digest);
}

/// 🚨 The row's whole reason. Different bytes behind the same name is `Changed`,
/// it carries what was pinned so the operator can tell a re-download from
/// something else, and the digests differ.
#[test]
fn different_bytes_behind_the_same_name_are_reported_as_changed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"the model that was pulled");
    let mut pins = Pins::default();
    let first = check(&mut pins, &model, Effort::Full, 1);
    let pinned = first.digest.clone().expect("a digest");

    touch(&model, b"a different model, same name");
    let second = check(&mut pins, &model, Effort::Full, 2);

    assert_eq!(second.outcome, WeightsOutcome::Changed { was: pinned });
    assert!(second.alarming());
    assert_ne!(second.digest, first.digest);
}

/// 🚨 **A mismatch does NOT re-pin, and this is the test with teeth.** If the
/// substitute became the reference, the alarm would fire exactly once and every
/// run after it would report `Unchanged` about the wrong file — a control that
/// disarms itself the first time it is right.
#[test]
fn a_mismatch_leaves_the_pin_alone_so_the_alarm_keeps_firing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"the model that was pulled");
    let mut pins = Pins::default();
    let pinned = check(&mut pins, &model, Effort::Full, 1)
        .digest
        .expect("a digest");

    touch(&model, b"a different model, same name");
    for round in 2..=4 {
        let checked = check(&mut pins, &model, Effort::Full, round);
        assert_eq!(
            checked.outcome,
            WeightsOutcome::Changed {
                was: pinned.clone()
            },
            "the alarm stopped firing on round {round}"
        );
        assert!(checked.pin.is_none(), "round {round} rewrote the pin");
    }
    assert_eq!(pins.get(&model).expect("still pinned").digest, pinned);
}

/// `--repin` is the only way back, and it goes through the same `check` so there
/// is one recipe for what a pin contains (F330).
#[test]
fn forgetting_a_pin_makes_the_next_check_a_first_sight_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"the model that was pulled");
    let mut pins = Pins::default();
    check(&mut pins, &model, Effort::Full, 1);
    touch(&model, b"a deliberate re-download");

    pins.forget(&model);
    let checked = check(&mut pins, &model, Effort::Full, 2);

    assert_eq!(checked.outcome, WeightsOutcome::Pinned);
    assert!(!checked.alarming());
    assert_eq!(pins.len(), 1);
}

/// ⚠ A file whose contents changed but whose length and timestamp did not is the
/// documented hole in the cheap check, and it is asserted rather than left to be
/// discovered — a limit nobody wrote down is a limit somebody will assume away.
/// `Effort::Full` is what closes it, which is what `abcc weights --verify` is
/// for.
#[test]
fn the_cheap_check_misses_a_same_length_same_time_rewrite_and_the_full_one_does_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"aaaaaaaaaaaaaaaa");
    let mut pins = Pins::default();
    check(&mut pins, &model, Effort::Full, 1);
    let when = fs::metadata(&model)
        .expect("stat")
        .modified()
        .expect("mtime");

    // Same length, and the timestamp put back the way an attacker would.
    touch(&model, b"bbbbbbbbbbbbbbbb");
    fs::File::options()
        .write(true)
        .open(&model)
        .expect("reopen")
        .set_modified(when)
        .expect("restore the timestamp");

    let cheap = check(&mut pins, &model, Effort::Cheap, 2);
    assert_eq!(
        cheap.outcome,
        WeightsOutcome::Unchanged,
        "the cheap check is documented to miss this; if it now catches it, say so"
    );

    let full = check(&mut pins, &model, Effort::Full, 3);
    assert!(
        matches!(full.outcome, WeightsOutcome::Changed { .. }),
        "the full check missed a rewrite: {:?}",
        full.outcome
    );
}

/// 🚨 A file that is not there is `Unlocated` and never a pass. A control that
/// cannot find its subject has to say so — silence reads exactly like a build
/// that predates the check.
#[test]
fn a_model_with_no_file_behind_it_is_unlocated_and_not_a_pass() {
    let mut pins = Pins::default();
    let checked = check(&mut pins, "a-model-that-is-not-a-path", Effort::Cheap, 1);

    match &checked.outcome {
        WeightsOutcome::Unlocated { why } => {
            // The sentence has to be actionable: W7's whole complaint about the
            // donor's controls is that they were correct and unusable.
            assert!(
                why.contains("ABCC_MODEL_WEIGHTS"),
                "the operator is not told what to set: {why}"
            );
        }
        other => panic!("an absent file was not Unlocated: {other:?}"),
    }
    assert!(checked.digest.is_none());
    assert!(!checked.alarming(), "an absent file is not an alarm either");
    assert!(pins.is_empty(), "nothing should have been pinned");
}

/// ⚠ A filesystem that reports no modification time must never compare equal.
/// Two files of the same length would otherwise satisfy the cheap check, and the
/// cheap check's only job is to err towards doing the expensive one.
#[test]
fn a_missing_timestamp_is_never_a_match() {
    let with = Identity {
        len: 10,
        modified_ms: Some(5),
    };
    let without = Identity {
        len: 10,
        modified_ms: None,
    };

    assert!(with.looks_unchanged(&with));
    assert!(!without.looks_unchanged(&without));
    assert!(!without.looks_unchanged(&with));
    assert!(!with.looks_unchanged(&without));
}

/// The digest is a real SHA-256 over the whole file, chunked. The known vector
/// is here so a change to the chunking cannot quietly change what is recorded —
/// a pin file full of digests nobody can reproduce is worse than no pin file.
#[test]
fn the_digest_is_sha256_over_the_whole_file_however_it_is_chunked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("abc");
    fs::write(&path, b"abc").expect("write");
    assert_eq!(
        digest_of(&path).expect("digest"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );

    // 🚨 **Two files sharing their whole first chunk and differing after it.**
    // Found by mutation: a `digest_of` that hashed only the first 8 MiB passed a
    // test that hashed one file twice and compared it with itself, and passed a
    // length check on the hex — the digest of *something* is always 64
    // characters. The oracle has to be a file the mutant cannot tell apart.
    let head = vec![0x5au8; 8 * 1024 * 1024];
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    fs::write(&a, [head.clone(), vec![1u8; 1024]].concat()).expect("write");
    fs::write(&b, [head, vec![2u8; 1024]].concat()).expect("write");

    let (da, db) = (digest_of(&a).expect("a"), digest_of(&b).expect("b"));
    assert_eq!(da.len(), 64);
    assert_ne!(
        da, db,
        "the digest stopped at the first chunk: two files differing only after \
         8 MiB hashed the same"
    );
    assert_eq!(
        da,
        digest_of(&a).expect("a again"),
        "the digest is not stable"
    );
}

/// 🚨 A corrupt pin file is an error and never an empty set. Falling back to
/// "no pins" would re-pin on the next start and report `Pinned` — the one
/// outcome that reads like success.
#[test]
fn a_corrupt_pin_file_refuses_rather_than_reading_as_no_pins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = pin_path(dir.path());
    fs::write(&path, "{ this is not json").expect("write");

    assert!(Pins::load(&path).is_err());

    // But an absent one is genuinely empty, which is the ordinary first run.
    let fresh: PathBuf = pin_path(&dir.path().join("nothing-here"));
    assert!(Pins::load(&fresh).expect("absent is empty").is_empty());
}

/// A pin survives the round trip to disk, because the whole control is that what
/// was recorded last month is what is compared today.
#[test]
fn pins_survive_the_round_trip_to_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"the weights");
    let path = pin_path(&dir.path().join("nested").join("deeper"));

    let mut pins = Pins::default();
    check(&mut pins, &model, Effort::Full, 99);
    pins.save(&path).expect("save");

    let read = Pins::load(&path).expect("load");
    assert_eq!(read.get(&model), pins.get(&model));
    assert_eq!(read.get(&model).expect("pinned").pinned_at_ms, 99);
}

/// 🚨 **The guard against F392's shape.** `reads_the_file` answers *is this
/// about to cost 54 seconds* by re-deriving, because the caller has to print a
/// sentence before `check` runs rather than after. Two functions answering one
/// question is exactly the defect F392 names — so this asserts they agree on
/// every arm, and it is what fails if one of them is edited alone.
#[test]
fn the_will_it_read_predicate_agrees_with_what_the_check_actually_did() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = weights_file(dir.path(), b"the weights");
    let mut pins = Pins::default();

    // No pin at all: both efforts must read.
    for effort in [Effort::Cheap, Effort::Full] {
        assert!(
            reads_the_file(&pins, &model, effort),
            "an unpinned model was not predicted to read at {effort}"
        );
    }
    check(&mut pins, &model, Effort::Full, 1);

    // Pinned and untouched: cheap does not read, full does.
    assert!(!reads_the_file(&pins, &model, Effort::Cheap));
    assert!(reads_the_file(&pins, &model, Effort::Full));
    assert_eq!(
        check(&mut pins, &model, Effort::Cheap, 2).outcome,
        WeightsOutcome::Unchanged,
        "predicted no read, and the check read anyway"
    );

    // Pinned and moved: cheap reads too.
    touch(&model, b"the weights, but longer than before");
    assert!(
        reads_the_file(&pins, &model, Effort::Cheap),
        "a moved file was not predicted to read"
    );
    assert!(matches!(
        check(&mut pins, &model, Effort::Cheap, 3).outcome,
        WeightsOutcome::Changed { .. }
    ));

    // Nothing to read: predicted false, and `check` agrees by not reading.
    let absent = Pins::default();
    assert!(!reads_the_file(&absent, "not-a-path-at-all", Effort::Cheap));
}
