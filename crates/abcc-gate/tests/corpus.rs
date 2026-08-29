//! 🚨 **The corpora — the exit criterion, and the population `PLAN.md` §3 asks
//! for.**
//!
//! `PLAN.md` §3 wants **zero false fails on a correct-tree population**. Under
//! ADR-0017 a correct tree is one that could *land*. The 25-attempt population in
//! `tests/live.rs` contains **no correct trees at all** — the ladder refuses all
//! 25 — so that clause is satisfied vacuously, which is to say it is not
//! satisfied. This is the population that satisfies it for real.
//!
//! # What the corpora are, and why they need no model call
//!
//! `corpus/suites/q56` (56 tasks, five languages) and `corpus/suites/k` (3 tasks,
//! real multi-file python projects) each ship, per task:
//!
//! ```text
//! prompt.txt   the task in a user's voice, verbatim from the donor
//! task.toml    lang, and the donor gate's measured evidence
//! verify.sh    THE ANSWER KEY — hidden reviewer tests, written in at grade time
//! fixture/     a tree that is already a WRONG answer
//! refsol/      the solution file(s) of a RIGHT one — a partial overlay, not a tree
//! ```
//!
//! So the answer key is on disk and both halves of the measurement are free of a
//! model: `fixture` is the wrong tree and `fixture` overlaid with `refsol` is the
//! right one. **59 tasks, 118 measured trees.**
//!
//! ⚠ `refsol/` is an **overlay**. It carries no `Cargo.toml` and never the test
//! file. *After* is `fixture` copied and then overwritten by `refsol`'s files —
//! never `cp refsol after`, which would produce a tree with no manifest and no
//! tests that the gate would report faithfully as broken, reading exactly like a
//! real result.
//!
//! # The two pairs, and why the wrong one has the `before` it has
//!
//! ```text
//! CORRECT   fixture                      ->  fixture + refsol
//! WRONG     fixture minus the solution   ->  fixture
//! ```
//!
//! The correct pair is the ruled one: the tree the task ships, then the same tree
//! with the right answer in it. Its `after` is a tree that could land, and **any
//! `Headline::Red` on it is a false fail.**
//!
//! The wrong pair's `before` is the fixture with exactly the files `refsol/`
//! overlays *removed*, so that its patch reads *the model wrote the solution
//! file* — which is what the prompt asked for, and which is the same file set the
//! correct pair's patch touches. 🚨 **The reverse pair was rejected**: showing a
//! reviewer a prompt that says *implement `slugify`* beside a diff that *removes*
//! working slug handling is asking it about a mismatch rather than about a
//! defect. Nothing here is authored — a deletion is not content — and the only
//! difference between the two halves is what is in the solution file.
//!
//! # 🚨 Two things measured on disk that the corpus notes had wrong
//!
//! 1. **The fixtures are two tiers, not one.** The imported note says a fixture
//!    *"already compiles and passes the visible tests"*, which is the state
//!    `SPEC.md` §9's generated null-implementation stub was invented to simulate.
//!    Measured over all 59: **38 of the 51 runnable fixtures pass their own
//!    visible tests, and 13 are null-implementation stubs** — `// TODO:
//!    implement`, `raise NotImplementedError`, empty method bodies — which fail
//!    them. Both tiers fail the *hidden* tests, which is what the donor gate
//!    recorded, so the note is right about point 1 and wrong about the tier.
//!    The two behave differently here and the tables below keep them apart.
//! 2. **8 shell tasks ship no test file at all**, only `solution.sh`. They get no
//!    acceptance command, land `Unmeasured { NothingToRun }` and are
//!    `Unverified` — correctly. ⚠ A `bash`-based rung was **not** invented for
//!    them: `bash` on PATH here is the WSL relay and it fails at exit 1 (F492),
//!    so declaring one would manufacture false fails on eight correct trees,
//!    which is the exact quantity this file exists to count.
//!
//! # The profiles, and why they live here
//!
//! [`Toolchain::detect`] finds a profile on **14 of the 59 trees**: there is no
//! `pyproject.toml`, `pytest.ini`, `package.json` or `clippy.toml` anywhere in
//! either corpus, and the only witness that exists is `Cargo.toml`, in the 14
//! rust fixtures. The other 45 would land `Unmeasured { NoCheckerForArtifact }`,
//! which is the type being honest and is **not** a false fail — but it is not a
//! measurement either, and a run that produces 45 shrugs has not met the exit
//! criterion.
//!
//! The fix needs no new mechanism: [`Gate::with_toolchain`] already takes an
//! operator-configured profile, and this file builds one per `task.toml`'s
//! `lang`. 🚨 They are **not** added to [`abcc_engine::workspace::TOOLCHAINS`],
//! and that is the decision rather than the shortcut: that table is a claim about
//! *every repository* — a witness file, then a command — and these are corpus
//! fixtures with no manifest, so the claim `node test_basic.mjs` would be true of
//! this corpus and of nothing else. The instrument is the honest home for a
//! command that is true of one population.
//!
//! # Running it
//!
//! ```text
//! ABCC_CORPUS=D:\dev\ABCC_20_powerd_by_claudette\corpus \
//! ABCC_CORPUS_SCRATCH=D:\ac \
//! ABCC_CORPUS_OUT=D:\dev\ABCC_20_powerd_by_claudette\research\corpus-run \
//!   cargo test -p abcc-gate --test corpus -- --ignored --nocapture
//! ```
//!
//! ⚠ **Put the scratch somewhere short.** The trees are built, measured and
//! deleted one at a time, and a `target/` under a long prefix is how F519's
//! `MAX_PATH` failure was found; 123 characters was enough to break it.
//!
//! ⚠ **28 cold cargo builds.** `CARGO_TARGET_DIR` is not on
//! [`abcc_engine::child::ENV_ALLOWLIST`], so the gate may not share a build cache
//! (F356) and every rust tree builds into its own `target/`. Each tree is removed
//! before the next is built.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use abcc_core::outcome::{Headline, Outcome, Reading, Why};
use abcc_engine::workspace::{Standard, Toolchain};
use abcc_gate::{Gate, Measured};
use abcc_vcs::Repo;

// ---------------------------------------------------------------------------
// The profiles — one per `task.toml` `lang`
// ---------------------------------------------------------------------------

/// The cargo profile, spelled out rather than borrowed from `TOOLCHAINS[0]` so
/// that an edit to that table cannot silently change what this population was
/// measured under.
///
/// ⚠ It keeps the **standard**, deliberately, even though no tree in either
/// corpus carries a `clippy.toml`. `Gate::standard` asks
/// [`Standard::declared_at`] and gets `false`, so the rung is *absent* — and an
/// absence a mechanism computed is worth more than an absence this file assumed.
const RUST: Toolchain = Toolchain {
    name: "cargo",
    witnesses: &["Cargo.toml"],
    test: &["cargo", "test"],
    diagnostics: &["cargo", "check", "--all-targets"],
    reading: Reading::Cargo,
    standard: Some(Standard {
        witnesses: &["clippy.toml", ".clippy.toml"],
        command: &["cargo", "clippy", "--all-targets", "--", "-D", "warnings"],
    }),
};

/// ⚠ `python` and never `python3`: on this platform `python3` resolves to a
/// Microsoft Store stub that prints an installation notice instead of running
/// anything (F312). Measured working under the scrubbed environment a rung child
/// gets — `pytest` is in this interpreter's own `site-packages` rather than the
/// user one, so it survives `env_clear()` plus the allowlist.
const PYTHON: Toolchain = Toolchain {
    name: "corpus-python",
    witnesses: &[],
    test: &["python", "-m", "pytest", "-q"],
    diagnostics: &["python", "-m", "compileall", "-q", "."],
    reading: Reading::Python,
    standard: None,
};

/// The fixture's own visible test file, named because there is no manifest to
/// name it. ⚠ `node --test` is **not** used: its default glob does not match
/// `test_basic.mjs`, so it would discover nothing and exit 0 — a green rung that
/// measured nothing, which is F517's shape exactly.
const NODE: Toolchain = Toolchain {
    name: "corpus-node",
    witnesses: &[],
    test: &["node", "test_basic.mjs"],
    diagnostics: &["node", "--check", "solution.mjs"],
    reading: Reading::ExitOnly,
    standard: None,
};

/// Node 24 strips types natively, so the `.ts` fixtures need no second runtime —
/// measured, because a version that supports a flag is not a version that has it
/// on by default.
const TYPESCRIPT: Toolchain = Toolchain {
    name: "corpus-typescript",
    witnesses: &[],
    test: &["node", "test_basic.ts"],
    diagnostics: &[],
    reading: Reading::ExitOnly,
    standard: None,
};

/// 🚨 **An empty command, on purpose.** The 8 shell fixtures are `solution.sh`
/// and nothing else — there is no test to run, and the acceptance rung answers
/// `Unmeasured { NothingToRun }`, which is true. Inventing `bash solution.sh`
/// would be worse than useless: it is not a test, and `bash` here is the WSL
/// relay that fails at exit 1 (F492), so the rung would refuse eight correct
/// trees.
const SHELL: Toolchain = Toolchain {
    name: "corpus-shell",
    witnesses: &[],
    test: &[],
    diagnostics: &[],
    reading: Reading::ExitOnly,
    standard: None,
};

fn profile(lang: &str) -> Toolchain {
    match lang {
        "rust" => RUST,
        "python" => PYTHON,
        "node" => NODE,
        "typescript" => TYPESCRIPT,
        "shell" => SHELL,
        other => panic!("`{other}` is a lang no profile is declared for"),
    }
}

// ---------------------------------------------------------------------------
// Reading the corpus
// ---------------------------------------------------------------------------

/// Build output and interpreter caches, which are not part of any tree.
///
/// ⚠ One K fixture ships a `.pytest_cache/` and eleven `.pyc` files. They are
/// identical on both sides of every pair so they would not reach a diff, but they
/// would reach the *commit* and the file list a rung reports, and a `.pyc` in the
/// evidence an operator reads is noise a checkout never had.
const NOT_A_TREE: &[&str] = &["__pycache__", ".pytest_cache", "target", ".git"];

struct Task {
    /// `q56/Q01`, which is what the tables are keyed by.
    id: String,
    /// `q56-Q01` — the same identity, spelled for a filename.
    slug: String,
    dir: PathBuf,
    lang: String,
    title: String,
    prompt: String,
}

/// Read one `key = "value"` out of a generated `task.toml`.
///
/// ⚠ **This is not a TOML parser and does not pretend to be one.** `task.toml`'s
/// header is machine-generated with a fixed shape — `key`, spaces, `=`, spaces, a
/// double-quoted scalar — and the two keys read here (`lang`, `title`) are both
/// in it. Adding a TOML dependency to read two lines out of a file this crate
/// otherwise never touches would be a dependency the workspace carries forever
/// for one test.
fn scalar(toml: &str, key: &str) -> Option<String> {
    toml.lines().map(str::trim_end).find_map(|line| {
        let rest = line.strip_prefix(key)?;
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        rest.strip_suffix('"').map(str::to_owned)
    })
}

fn corpus_root() -> PathBuf {
    let raw = env::var_os("ABCC_CORPUS")
        .expect("set ABCC_CORPUS to the corpus directory — see the module docs");
    let root = PathBuf::from(raw);
    // Either the `corpus/` directory or its `suites/` child, because both are
    // things a person types.
    if root.join("suites").is_dir() {
        root.join("suites")
    } else {
        root
    }
}

fn tasks() -> Vec<Task> {
    let suites = corpus_root();
    let only = env::var("ABCC_CORPUS_ONLY").unwrap_or_default();
    let mut out = Vec::new();
    for suite in ["q56", "k"] {
        let dir = suites.join(suite).join("tasks");
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{} is not readable: {e}", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("task.toml").is_file())
            .collect();
        entries.sort();
        for dir in entries {
            let toml = fs::read_to_string(dir.join("task.toml")).expect("task.toml");
            let name = dir
                .file_name()
                .expect("a task directory has a name")
                .to_string_lossy()
                .into_owned();
            if !only.is_empty() && !only.split(',').any(|w| w.trim() == name) {
                continue;
            }
            out.push(Task {
                id: format!("{suite}/{name}"),
                slug: format!("{suite}-{name}"),
                lang: scalar(&toml, "lang")
                    .unwrap_or_else(|| panic!("{}/task.toml declares no lang", dir.display())),
                title: scalar(&toml, "title").unwrap_or_else(|| name.clone()),
                prompt: fs::read_to_string(dir.join("prompt.txt")).expect("prompt.txt"),
                dir,
            });
        }
    }
    assert!(!out.is_empty(), "no tasks under {}", suites.display());
    out
}

/// The paths `refsol/` overlays — which is the corpus's own answer to *which
/// files are the solution*, and is therefore what the wrong pair's `before`
/// removes.
fn solution_paths(task: &Task) -> Vec<PathBuf> {
    let refsol = task.dir.join("refsol");
    let mut out = Vec::new();
    walk(&refsol, &refsol, &mut out);
    out.sort();
    out
}

/// Whether this task ships a `sham/` — the corpus's own **local wrong answer**.
fn has_sham(task: &Task) -> bool {
    task.dir.join("sham").is_dir()
}

fn walk(dir: &Path, base: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if NOT_A_TREE.iter().any(|s| *s == entry.file_name()) {
            continue;
        }
        if path.is_dir() {
            walk(&path, base, into);
        } else if let Ok(rel) = path.strip_prefix(base) {
            into.push(rel.to_path_buf());
        }
    }
}

// ---------------------------------------------------------------------------
// Building one tree
// ---------------------------------------------------------------------------

fn copy_tree(from: &Path, to: &Path) {
    let mut files = Vec::new();
    walk(from, from, &mut files);
    for rel in files {
        let dst = to.join(&rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).expect("a directory for a copied file");
        }
        fs::copy(from.join(&rel), &dst).expect("copying a corpus file");
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Walked {
    measured: Measured,
    patch: String,
    seconds: f32,
}

/// Lay down `before`, snapshot it, lay down `after`, snapshot that, and walk the
/// ladder over the pair with the tree left standing at `after`.
///
/// 🚨 Both snapshots are taken **before any rung runs**, so no `target/` and no
/// `hidden_gate_test.py` can reach the diff. The rungs then run against the
/// working tree, which is `after` — which is the thing `headline_at` checks when
/// it asks whether the measurement's sha is the one being reported on.
///
/// The identity is set locally rather than relied on from the operator's global
/// config: `commit-tree` needs one, and a host that has none would fail here in a
/// way that reads like the corpus being wrong.
fn walk_pair(root: &Path, lang: &str, before: &dyn Fn(&Path), after: &dyn Fn(&Path)) -> Walked {
    fs::create_dir_all(root).expect("the scratch root");
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "corpus@abcc.invalid"]);
    git(root, &["config", "user.name", "abcc corpus"]);
    git(
        root,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "the empty tree both snapshots are taken against",
        ],
    );
    let repo = Repo::open(root).expect("the scratch repository");

    before(root);
    let opening = repo
        .checkpoint(
            "refs/abcc/corpus/before",
            "the tree the attempt started from",
        )
        .expect("the opening checkpoint");
    after(root);
    let closing = repo
        .checkpoint("refs/abcc/corpus/after", "the tree the attempt left")
        .expect("the closing checkpoint");

    let patch = repo.patch_between(&opening, &closing).unwrap_or_default();
    let gate = Gate::open(&repo, root).with_toolchain(profile(lang));
    let started = std::time::Instant::now();
    let measured = gate.measure(&opening, &closing);
    Walked {
        measured,
        patch,
        seconds: started.elapsed().as_secs_f32(),
    }
}

/// Remove a measured tree before the next one is built. A `target/` per rust tree
/// is the price of not sharing a build cache (F356) and they do not fit side by
/// side.
fn discard(root: &Path) {
    if root.exists() && fs::remove_dir_all(root).is_err() {
        // Second pass: cargo leaves read-only files in `target/` on this platform
        // often enough that one retry is worth more than a warning nobody reads.
        if let Err(e) = fs::remove_dir_all(root) {
            println!("    ⚠ {} would not be removed: {e}", root.display());
        }
    }
}

fn scratch_root() -> PathBuf {
    env::var_os("ABCC_CORPUS_SCRATCH")
        .map_or_else(|| env::temp_dir().join("abcc-corpus"), PathBuf::from)
}

fn out_dir() -> Option<PathBuf> {
    let dir = env::var_os("ABCC_CORPUS_OUT").map(PathBuf::from)?;
    fs::create_dir_all(&dir).expect("the output directory");
    Some(dir)
}

/// One line per rung, and the headline, in the shape the tables use.
fn show(walked: &Walked) {
    println!("    headline: {}", walked.measured.headline);
    for outcome in walked.measured.report.outcomes() {
        match outcome {
            Outcome::Measured(m) => println!(
                "      {:<11} exit {:<4} {}",
                m.rung,
                m.exit,
                first_line(&m.detail)
            ),
            Outcome::Unmeasured { rung, why } => {
                println!("      {rung:<11} ---      unmeasured: {why}");
            }
        }
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(96) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

fn verdict(headline: &Headline) -> &'static str {
    match headline {
        Headline::Green { .. } => "GREEN",
        Headline::Red { .. } => "RED",
        Headline::Unverified { .. } => "UNVERIFIED",
    }
}

/// Why a tree was not measured, as one phrase an operator can group by.
fn absence(headline: &Headline) -> String {
    match headline {
        Headline::Unverified { missing } => missing
            .iter()
            .map(|(rung, why)| {
                let what = match why {
                    Why::NothingToRun { .. } => "nothing-to-run",
                    Why::NoCheckerForArtifact { .. } => "no-checker",
                    Why::CheckerNotOnHost { .. } => "checker-absent",
                    Why::SpawnFailed { .. } => "spawn-failed",
                    Why::FailedBeforeRunning { .. } => "failed-before-running",
                    Why::Timeout { .. } => "timeout",
                    _ => "other",
                };
                format!("{rung}:{what}")
            })
            .collect::<Vec<_>>()
            .join(" "),
        Headline::Red { rung, .. } => format!("refused-by:{rung}"),
        Headline::Green { .. } => String::new(),
    }
}

/// One measured tree, as the tables carry it.
struct Row {
    id: String,
    lang: String,
    verdict: &'static str,
    detail: String,
    seconds: f32,
}

// ---------------------------------------------------------------------------
// The exit criterion
// ---------------------------------------------------------------------------

/// 🚨 **`PLAN.md` §3: zero false fails on a correct-tree population.**
///
/// One row per task. `fixture` is the tree the task ships and `fixture` overlaid
/// with `refsol` is the answer key's right answer, so **every `Red` here is a
/// false fail** and the assertion at the end is the criterion itself.
///
/// ⚠ `Unverified` is neither a pass nor a fail — `Headline::is_pass` is `Green`
/// and nothing else — so the table counts the three separately and the summary
/// says how much of the population was measurable at all. A criterion met by 51
/// greens and 8 shrugs is a different sentence from one met by 59 greens, and
/// collapsing them is the vacuity this file exists to escape.
#[test]
#[ignore = "needs the corpora on disk and a cold build per rust tree"]
fn the_gate_refuses_no_correct_tree_the_corpora_ship() {
    let tasks = tasks();
    let scratch = scratch_root();
    let out = out_dir();
    let mut rows: Vec<Row> = Vec::new();
    let mut false_fails: Vec<String> = Vec::new();

    println!(
        "\n### CORRECT TREES — fixture -> fixture+refsol, {} tasks\n",
        tasks.len()
    );
    for task in &tasks {
        let root = scratch.join(&task.slug).join("correct");
        discard(&root);
        let fixture = task.dir.join("fixture");
        let refsol = task.dir.join("refsol");
        let walked = walk_pair(
            &root,
            &task.lang,
            &|tree| copy_tree(&fixture, tree),
            &|tree| copy_tree(&refsol, tree),
        );
        println!("=== {} [{}] ({:.1} s)", task.id, task.lang, walked.seconds);
        show(&walked);
        if let Headline::Red { rung, detail } = &walked.measured.headline {
            false_fails.push(format!("{} ({rung}: {})", task.id, first_line(detail)));
        }
        rows.push(Row {
            id: task.id.clone(),
            lang: task.lang.clone(),
            verdict: verdict(&walked.measured.headline),
            detail: absence(&walked.measured.headline),
            seconds: walked.seconds,
        });
        discard(&root);
    }

    summarise("CORRECT TREES", &rows);
    if let Some(dir) = &out {
        write_tsv(&dir.join("correct.tsv"), &rows);
    }

    assert!(
        false_fails.is_empty(),
        "\n🚨 {} FALSE FAIL(S) — the gate refused a tree the answer key says is right:\n  {}\n",
        false_fails.len(),
        false_fails.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// The other half
// ---------------------------------------------------------------------------

/// **What the deterministic ladder says about a tree that is wrong** — and the
/// one number ADR-0008's Judge has no evidence for.
///
/// The pair is *the fixture with its solution file(s) removed* → *the fixture*,
/// so the patch reads as the model writing the solution the prompt asked for, and
/// the tree it leaves is one the hidden reviewer tests fail.
///
/// 🚨 **A `GREEN` row here is the whole argument for the Judge.** The donor's
/// central design is that the visible tests are happy-path and a wrong answer
/// passes them, so a green row is a tree the deterministic gate accepts and the
/// answer key rejects — and the only thing left that could see it is a reviewer
/// reading the diff. **There is no assertion in this test**, because there is no
/// number here that would be a defect: a red row is the ladder working and a
/// green row is the population the Judge exists for. It counts, and counting is
/// all it does.
///
/// With `ABCC_CORPUS_OUT` set it also writes one dossier per tree — the title,
/// the prompt, the patch and the measurements — which is exactly
/// `abcc_gate::judge::Dossier`'s field list and is what the live review stage
/// reads.
#[test]
#[ignore = "needs the corpora on disk and a cold build per rust tree"]
fn what_the_ladder_says_about_the_wrong_trees_the_corpora_ship() {
    let tasks = tasks();
    let scratch = scratch_root();
    let out = out_dir();
    let mut rows: Vec<Row> = Vec::new();
    let mut green_but_wrong: Vec<String> = Vec::new();

    println!(
        "\n### WRONG TREES — fixture minus the solution -> fixture, {} tasks\n",
        tasks.len()
    );
    for task in &tasks {
        let root = scratch.join(&task.slug).join("wrong");
        discard(&root);
        let fixture = task.dir.join("fixture");
        let solution = solution_paths(task);
        let walked = walk_pair(
            &root,
            &task.lang,
            &|tree| {
                copy_tree(&fixture, tree);
                for rel in &solution {
                    let _ = fs::remove_file(tree.join(rel));
                }
            },
            &|tree| copy_tree(&fixture, tree),
        );
        println!("=== {} [{}] ({:.1} s)", task.id, task.lang, walked.seconds);
        show(&walked);
        if walked.measured.accepts() {
            green_but_wrong.push(task.id.clone());
        }
        if let Some(dir) = &out {
            write_dossier(dir, task, &walked);
        }
        rows.push(Row {
            id: task.id.clone(),
            lang: task.lang.clone(),
            verdict: verdict(&walked.measured.headline),
            detail: absence(&walked.measured.headline),
            seconds: walked.seconds,
        });
        discard(&root);
    }

    summarise("WRONG TREES", &rows);
    println!(
        "\n🚨 GREEN ON A WRONG TREE: {} of {} — the deterministic ladder accepts them and the \
         answer key does not.\n  {}",
        green_but_wrong.len(),
        rows.len(),
        green_but_wrong.join(", ")
    );
    if let Some(dir) = &out {
        write_tsv(&dir.join("wrong.tsv"), &rows);
    }
}

// ---------------------------------------------------------------------------
// The hardest tier the corpora ship
// ---------------------------------------------------------------------------

/// 🚨 **The shams — `fixture` → `fixture`+`sham`, and this is the tier the whole
/// argument turns on.**
///
/// Only the K suite ships one (`point3 = "sound"` there, `"not_run"` in Q56), so
/// it is **3 trees**. They are worth their own test anyway, because a sham is
/// neither of the other two things: it is not a stub and it is not a naive first
/// attempt, it is **the tempting local wrong answer** — a change that fixes the
/// symptom the ticket reported and leaves the defect. The corpus says so in its
/// own words:
///
/// * `finish_the_cancelled_status` — *"fixing `sla.py` alone … fixes exactly what
///   the ticket described — SLA breaches drop from 6 to 2 — and leaves the
///   service billing customers for four cancelled jobs and requeueing work an
///   operator told it to stop."*
/// * `round_at_the_line_not_the_total` — *"changing the rounding direction at the
///   end — ceil instead of half-up — FIXES the reported invoice, which is what
///   makes it tempting. It fails because the eight wrong totals differ in BOTH
///   directions."*
///
/// 🚨 **The pair is deliberately the same shape as the correct one**: same
/// `before`, an overlay on top, and for `round_at_the_line` **literally the same
/// file** (`billing/pricing.py`) in both. So the sham row and the correct row for
/// one task differ in exactly one thing — what the solution file says — which is
/// as close to a controlled A/B as a corpus gets.
///
/// ⚠ There is no assertion, for the wrong half's reason. A `Green` here is the
/// deterministic ladder accepting a change the answer key rejects, and it is
/// expected: the sham passes the visible tests, which is what makes it tempting.
#[test]
#[ignore = "needs the corpora on disk"]
fn what_the_ladder_says_about_the_shams_the_k_suite_ships() {
    let tasks: Vec<Task> = tasks().into_iter().filter(has_sham).collect();
    assert!(!tasks.is_empty(), "no task in either corpus ships a sham/");
    let scratch = scratch_root();
    let out = out_dir();
    let mut rows: Vec<Row> = Vec::new();

    println!(
        "\n### SHAMS — fixture -> fixture+sham, {} task(s)\n",
        tasks.len()
    );
    for task in &tasks {
        let root = scratch.join(&task.slug).join("sham");
        discard(&root);
        let fixture = task.dir.join("fixture");
        let sham = task.dir.join("sham");
        let walked = walk_pair(
            &root,
            &task.lang,
            &|tree| copy_tree(&fixture, tree),
            &|tree| copy_tree(&sham, tree),
        );
        println!("=== {} [{}] ({:.1} s)", task.id, task.lang, walked.seconds);
        show(&walked);
        if let Some(dir) = &out {
            // Its own slug, so the review stage picks it up beside the wrong
            // trees rather than overwriting one.
            let named = Task {
                slug: format!("{}-sham", task.slug),
                id: format!("{} (sham)", task.id),
                dir: task.dir.clone(),
                lang: task.lang.clone(),
                title: task.title.clone(),
                prompt: task.prompt.clone(),
            };
            write_dossier(dir, &named, &walked);
        }
        rows.push(Row {
            id: task.id.clone(),
            lang: task.lang.clone(),
            verdict: verdict(&walked.measured.headline),
            detail: absence(&walked.measured.headline),
            seconds: walked.seconds,
        });
        discard(&root);
    }

    summarise("SHAMS", &rows);
    if let Some(dir) = &out {
        write_tsv(&dir.join("sham.tsv"), &rows);
    }
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn summarise(what: &str, rows: &[Row]) {
    let mut by_lang: BTreeMap<&str, [usize; 3]> = BTreeMap::new();
    let mut absences: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        let slot = by_lang.entry(row.lang.as_str()).or_default();
        match row.verdict {
            "GREEN" => slot[0] += 1,
            "RED" => slot[1] += 1,
            _ => slot[2] += 1,
        }
        if !row.detail.is_empty() {
            *absences.entry(row.detail.as_str()).or_default() += 1;
        }
    }
    let total: f32 = rows.iter().map(|r| r.seconds).sum();
    println!("\n--- {what} ---");
    println!(
        "{:<12} {:>6} {:>6} {:>11}",
        "lang", "green", "red", "unverified"
    );
    for (lang, [g, r, u]) in &by_lang {
        println!("{lang:<12} {g:>6} {r:>6} {u:>11}");
    }
    let g: usize = by_lang.values().map(|s| s[0]).sum();
    let r: usize = by_lang.values().map(|s| s[1]).sum();
    let u: usize = by_lang.values().map(|s| s[2]).sum();
    println!(
        "{:<12} {g:>6} {r:>6} {u:>11}   ({} trees, {total:.0} s)",
        "TOTAL",
        rows.len()
    );
    if !absences.is_empty() {
        println!("\nwhat was missing, and how often:");
        for (why, n) in &absences {
            println!("  {n:>3}  {why}");
        }
    }
}

fn write_tsv(path: &Path, rows: &[Row]) {
    let mut s = String::from("task\tlang\tverdict\tdetail\tseconds\n");
    for row in rows {
        let _ = writeln!(
            s,
            "{}\t{}\t{}\t{}\t{:.2}",
            row.id, row.lang, row.verdict, row.detail, row.seconds
        );
    }
    fs::write(path, s).expect("the summary table");
    println!("\nwritten: {}", path.display());
}

/// One tree's dossier, in `judge::Dossier`'s field list plus the identity.
///
/// ⚠ The report is serialized rather than rendered, so the review stage shows the
/// Judge the same `Outcome`s the ladder produced rather than a second rendering
/// of them that could drift.
fn write_dossier(dir: &Path, task: &Task, walked: &Walked) {
    let dossiers = dir.join("dossiers");
    fs::create_dir_all(&dossiers).expect("the dossier directory");
    let body = serde_json::json!({
        "id": task.id,
        "lang": task.lang,
        "title": task.title,
        "prompt": task.prompt,
        "patch": walked.patch,
        "report": walked.measured.report,
        "headline": walked.measured.headline,
    });
    fs::write(
        dossiers.join(format!("{}.json", task.slug)),
        serde_json::to_string_pretty(&body).expect("a dossier serializes"),
    )
    .expect("writing a dossier");
}
