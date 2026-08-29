//! 🚨 **The Gate's acceptance test, and it was already in the log before the
//! Gate existed.**
//!
//! Twenty-five real attempts by the champion on one task — *"add a `--version`
//! flag to the abcc binary"* — are on this project's own event log, each one
//! bracketed by an opening and a closing checkpoint. Nine of them changed the
//! tree. Six of those nine compile and print `abcc 0.1.0`; **the repository's own
//! declared standard refuses all six** (F512), and on two of them every test
//! passes, so the standard is their only barrier. Two more changed the tree and
//! **do not compile at all**, because they ended mid-edit (F516).
//!
//! That is a population with an answer key, produced by a real model against a
//! real repository, and no part of it was written to make a gate look good. So it
//! is what the ladder is put to.
//!
//! # Running it
//!
//! ```text
//! ABCC_GATE_PAIRS="6b59bd9f32:491f07f2f9,ae1c895b8b:e6985430a2" \
//!   cargo test -p abcc-gate --test live -- --ignored --nocapture
//! ```
//!
//! `ABCC_GATE_PAIRS` is `before:after` pairs, comma separated. `ABCC_GATE_REPO`
//! points at the repository holding them; it defaults to this workspace.
//!
//! ⚠ **It is `#[ignore]`d and it is expensive.** Every pair whose tree changed
//! gets a fresh worktree with a fresh `target/`, because the gate may not share a
//! build cache (F356) — measured at **55 s and 2.3 GB** for one cold
//! `cargo test --workspace` here. Sixteen of the twenty-five pairs cost nothing
//! at all, because the free rung refuses them first.

use std::env;
use std::path::{Path, PathBuf};

use abcc_gate::Gate;
use abcc_vcs::{Repo, Sha};

fn repo_root() -> PathBuf {
    env::var_os("ABCC_GATE_REPO").map_or_else(
        || {
            // The crate directory is `<workspace>/crates/abcc-gate`.
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .expect("the workspace root is two above the crate")
                .to_path_buf()
        },
        PathBuf::from,
    )
}

/// One row per pair: what the ladder said, and what it cost.
#[test]
#[ignore = "needs real checkpoints and a cold cargo build per changed tree"]
fn the_gate_refuses_every_attempt_this_project_has_on_its_log() {
    let Some(spec) = env::var_os("ABCC_GATE_PAIRS") else {
        panic!("set ABCC_GATE_PAIRS=\"before:after,before:after\" — see the module docs");
    };
    let spec = spec.to_string_lossy().into_owned();
    let root = repo_root();
    let repo = Repo::open(&root).expect("the repository");
    let scratch = tempfile::tempdir().expect("tempdir");

    let mut accepted = Vec::new();
    for (n, pair) in spec.split(',').filter(|s| !s.trim().is_empty()).enumerate() {
        let (before, after) = pair
            .trim()
            .split_once(':')
            .unwrap_or_else(|| panic!("`{pair}` is not `before:after`"));
        let before = Sha::parse(before).expect("a before sha");
        let after = Sha::parse(after).expect("an after sha");

        let path = scratch.path().join(format!("pair-{n}"));
        let worktree = repo
            .open_worktree(&path, &after)
            .expect("a worktree at the closing checkpoint");
        // 🚨 Opened on the worktree, which is where the checkers run and which
        // shares the git directory, so both shas still resolve.
        let inner = Repo::open(worktree.path()).expect("the worktree's repository");

        let gate = Gate::open(&inner, worktree.path());
        let started = std::time::Instant::now();
        let measured = gate.measure(&before, &after);
        let elapsed = started.elapsed();

        println!(
            "\n=== {} .. {}  ({} rung(s) declared, {} reached, {:.1} s)",
            before.short(),
            after.short(),
            gate.ladder().len(),
            measured.report.outcomes().len(),
            elapsed.as_secs_f32()
        );
        println!("    headline: {}", measured.headline);
        for outcome in measured.report.outcomes() {
            println!("    {outcome:?}");
        }
        if measured.accepts() {
            accepted.push(format!("{}..{}", before.short(), after.short()));
        }

        // Removed before the next pair, because the price of not sharing a build
        // cache is a `target/` per tree and they do not fit side by side.
        let path = worktree.path().display().to_string();
        if let Err(e) = worktree.close() {
            println!("    ⚠ the worktree at {path} would not close: {e}");
        }
    }

    assert!(
        accepted.is_empty(),
        "the gate accepted {} of this population, and there is nothing in it that should pass: {}",
        accepted.len(),
        accepted.join(", ")
    );
}
