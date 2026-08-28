//! What the checkpoint cycle costs on a real repository on this machine.
//!
//! ADR-0007 quotes **take 0.16 s · diff 0.024 s · hand to an isolated attempt
//! 0.25 s · restore 0.06 s**, measured on four real repositories, with the
//! snapshot at 0.16 s on a 495-file tree and 0.31 s on a 1,079-file one. The
//! whole isolation decision rests on the marker being free and the working
//! directory not being free, so the number is worth being able to re-derive
//! rather than inherit.
//!
//! `#[ignore]`d because it writes worktrees and refs into a real repository:
//!
//! ```text
//! cargo test -p abcc-vcs --test cycle_cost -- --ignored --nocapture
//! ABCC_CYCLE_REPO=D:/dev/claudette cargo test -p abcc-vcs --test cycle_cost -- --ignored --nocapture
//! ```
//!
//! It cleans up after itself: the worktree is closed and every ref it wrote is
//! deleted, so running it against a live repository leaves nothing behind.
//!
//! # F490 — what the snapshot cost actually tracks, measured 2026-08-28
//!
//! | repository | files in tree | tracked | take (warm) | diff | hand over |
//! |---|---|---|---|---|---|
//! | this workspace | ~60 | 54 (0.2 MB) | 165 ms | 22 ms | 86 ms |
//! | the research repo | **71,003** | 1,585 (21.4 MB) | **1,167 ms** | 21 ms | 1,011 ms |
//!
//! ADR-0007's figures (0.16 s take, 0.024 s diff, 0.25 s hand over) reproduce on
//! a small tree. On a large one the snapshot costs 7x more, and the ADR's stated
//! falsifier is *"a subject repository where the temp-index snapshot is not
//! cheap"* — so the cause is worth attributing rather than assuming.
//!
//! 🚨 **It is git's untracked-file walk, and it is a function of how many files
//! are in the tree at all — not of how many are tracked, how many bytes they
//! hold, or how the ignore rules are written.** Three plausible causes were
//! measured and rejected first:
//!
//! * **Ignored-file count** — 20,000 ignored files added ~0 ms in a synthetic
//!   repo. (Measured at a scale where the ~150 ms fixed cost buried the signal;
//!   the effect is real and only shows up an order of magnitude further out.)
//! * **Tracked bytes** — 1,600 files holding 21 MB cost 296 ms against 245 ms for
//!   the same 1,600 files holding 0.2 MB. Fifty milliseconds for a hundredfold.
//! * **`.gitignore` negation defeating directory pruning** — the research repo
//!   re-includes two paths under an excluded tree, which stops git pruning the
//!   subtree. Worth 40 ms over 20,040 files.
//!
//! The control that settles it: on the same 71,003-file tree, `git status` costs
//! **822–941 ms** — the same order as the whole snapshot — while `git ls-files`,
//! which answers from the index and never walks, costs **56 ms**.
//!
//! **What this means for the design.** Nothing changes: 1.2 s is still free
//! against a 23.77 s model swap and a 24.7 s warm build, and the diff — the
//! gate's structural rung — does not move at all, because it reads two trees and
//! never touches the working directory. What it does say is where the ceiling is,
//! and that the lever is git's own untracked cache or fsmonitor rather than
//! anything this crate would write. ▶ Revisit if a subject repository ever pushes
//! the take past a few seconds.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use abcc_vcs::{Repo, checkpoint_ref};

const MISSION: &str = "cycle-cost-probe";

#[test]
#[ignore = "writes into a real repository; run explicitly"]
fn measure_the_checkpoint_cycle() {
    let root = std::env::var("ABCC_CYCLE_REPO").map_or_else(
        |_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .to_path_buf()
        },
        PathBuf::from,
    );
    let repo = Repo::open(&root).expect("open");
    println!("repository: {}", repo.root().display());

    let tracked = Command::new("git")
        .current_dir(repo.root())
        .args(["ls-files"])
        .output()
        .expect("ls-files");
    let files = String::from_utf8_lossy(&tracked.stdout).lines().count();
    println!("{files} tracked files");

    let start = Instant::now();
    let first = repo
        .checkpoint(&checkpoint_ref(MISSION, 1), "cycle cost: take")
        .expect("checkpoint");
    let take = start.elapsed();

    let start = Instant::now();
    let second = repo
        .checkpoint(&checkpoint_ref(MISSION, 2), "cycle cost: take again")
        .expect("checkpoint");
    let take_warm = start.elapsed();

    let start = Instant::now();
    let changes = repo.changed_between(&first, &second).expect("diff");
    let diff = start.elapsed();

    let wt_path = std::env::temp_dir().join(format!("abcc-cycle-{}", std::process::id()));
    let start = Instant::now();
    let wt = repo.open_worktree(&wt_path, &first).expect("worktree");
    let hand_over = start.elapsed();

    println!("take        {take:>10.3?}  (cold index)");
    println!("take again  {take_warm:>10.3?}");
    println!("diff        {diff:>10.3?}  ({} paths)", changes.len());
    println!("hand over   {hand_over:>10.3?}");

    wt.close().expect("close");

    // Leave the repository as it was found.
    for seq in [1, 2] {
        let out = Command::new("git")
            .current_dir(repo.root())
            .args(["update-ref", "-d", &checkpoint_ref(MISSION, seq)])
            .output()
            .expect("update-ref -d");
        assert!(
            out.status.success(),
            "could not delete the probe ref: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The claim under test is that the marker is cheap. A snapshot that took
    // longer than a model's time-to-first-token would change the design, not the
    // number.
    assert!(
        take_warm.as_secs_f64() < 10.0,
        "the snapshot is no longer free: {take_warm:?}"
    );
}
